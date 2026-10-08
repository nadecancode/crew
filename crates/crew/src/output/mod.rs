//! Explicit views of caller-captured, already-visible tool output. No command
//! execution, shell rewriting, source deletion, credential discovery or hooks.
//! Redaction must happen upstream, exactly as for the original tool result.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Args, Subcommand, ValueEnum};
use crew_core::client::Client;
use crew_core::output_view::{self, CapturedStatus, InputFormat, MAX_CAPTURE_BYTES};
use crew_core::paths::{self, Identity};
use crew_core::rpc::Role;
use serde::Serialize;

pub(crate) mod binary;
mod store;

const PREVIEW_BYTES: usize = 16 * 1024;
const MAX_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Args)]
pub struct OutputArgs {
    /// Active member identity (default: $CREW_IDENTITY); never operator access
    #[arg(long, global = true, value_name = "FILE")]
    identity: Option<PathBuf>,
    /// Require the exact executable selected by an immutable toolbox
    #[arg(long, global = true, value_name = "SHA256")]
    expected_helper: Option<String>,
    #[command(subcommand)]
    command: OutputCommand,
}

#[derive(Subcommand)]
enum OutputCommand {
    /// View captured output and retain exact raw bytes; never runs a command
    View(ViewArgs),
    /// Recover an exact byte range to stdout; metadata goes to stderr
    Raw(RawArgs),
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("status").required(true).multiple(true).args(["exit_code", "signal", "timed_out"])))]
struct ViewArgs {
    /// Already captured stdout (or a combined log), after original redaction
    input: PathBuf,
    /// Separately captured stderr, after original redaction
    #[arg(long, value_name = "FILE")]
    stderr: Option<PathBuf>,
    /// Original command's exit code, retained as data; not the helper's status
    #[arg(long, conflicts_with = "signal", value_parser = clap::value_parser!(i32).range(0..=255))]
    exit_code: Option<i32>,
    /// Original command's terminating signal
    #[arg(long, conflicts_with = "exit_code", value_parser = clap::value_parser!(i32).range(1..=127))]
    signal: Option<i32>,
    /// Original command timed out (may accompany an exit code or signal)
    #[arg(long)]
    timed_out: bool,
    /// Machine data and explicit raw mode always bypass summarization
    #[arg(long, value_enum, default_value_t = Format::CargoTest)]
    input_format: Format,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    CargoTest,
    Raw,
    Machine,
}

#[derive(Args)]
struct RawArgs {
    /// Content reference emitted by view; resolved only within this member
    reference: String,
    #[arg(long, value_enum, default_value_t = Stream::Stdout)]
    stream: Stream,
    /// Start byte, not a line number
    #[arg(long, default_value_t = 0)]
    offset: u64,
    /// Maximum bytes to recover (at most 1 MiB)
    #[arg(long, default_value_t = 65536, value_parser = clap::value_parser!(u32).range(1..=1048576))]
    length: u32,
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stream {
    Stdout,
    Stderr,
}

struct Member {
    root: File,
    identity: Identity,
    // Keep the authenticated connection alive for this bounded operation.
    _client: Client,
}

impl Member {
    fn authenticate(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os(paths::IDENTITY_ENV).map(PathBuf::from))
            .context(
                "output commands require an active member identity (--identity or CREW_IDENTITY)",
            )?;
        if path.file_name().is_none_or(|name| name != "identity.json") {
            bail!("output commands require the member's identity.json");
        }
        let parent = path.parent().context("identity has no member directory")?;
        let root = File::from(
            crate::os::open_no_symlinks(
                libc::AT_FDCWD,
                parent,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
                false,
            )
            .context("cannot open member identity directory without following links")?,
        );
        let meta = root.metadata()?;
        if meta.uid() != paths::uid() || meta.permissions().mode() & 0o777 != 0o700 {
            bail!("member identity directory must be private and owned by the current user");
        }
        let file = store::open_at(&root, "identity.json", libc::O_RDONLY, 0)?;
        store::private_file(&file)?;
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("member identity is oversized");
        }
        let identity: Identity = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid member identity"))?;
        if !crew_core::util::is_valid_id(&identity.agent_id)
            || identity.token.is_empty()
            || parent
                .file_name()
                .is_none_or(|name| name != identity.agent_id.as_str())
            || parent
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|name| name != "agents")
        {
            bail!("identity does not name its member directory");
        }
        let (client, hello) = Client::agent(&identity, "output")?;
        if hello.role != Role::Agent || hello.agent_id.as_deref() != Some(&identity.agent_id) {
            bail!("daemon did not authenticate this member");
        }
        Ok(Self {
            root,
            identity,
            _client: client,
        })
    }

    fn store(&self) -> Result<store::Store> {
        store::Store::open(&self.root, store::namespace(&self.identity))
    }
}

/// Helper success/failure is independent of the original command's status.
pub fn run(args: OutputArgs) -> Result<()> {
    if let Some(expected) = &args.expected_helper {
        binary::verify_running(expected)?;
    }
    // Authentication precedes reading source bytes or opening the raw store.
    let member = Member::authenticate(args.identity.as_deref())?;
    match args.command {
        OutputCommand::View(args) => show(&member, args),
        OutputCommand::Raw(args) => recover(&member, args),
    }
}

#[derive(Serialize)]
struct Preview {
    stream: Stream,
    start: usize,
    end: usize,
    total_bytes: usize,
    encoding: &'static str,
    data: String,
    truncated: bool,
}

fn preview(stream: Stream, bytes: &[u8]) -> Preview {
    let end = bytes.len().min(PREVIEW_BYTES);
    let (encoding, data) = match std::str::from_utf8(&bytes[..end]) {
        Ok(text) => ("utf8", text.to_owned()),
        Err(_) => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(&bytes[..end]),
        ),
    };
    Preview {
        stream,
        start: 0,
        end,
        total_bytes: bytes.len(),
        encoding,
        data,
        truncated: end < bytes.len(),
    }
}

#[derive(Serialize)]
struct Report {
    #[serde(flatten)]
    view: output_view::OutputView,
    raw_retained: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_error: Option<&'static str>,
    /// The source has already passed the original tool's redaction policy.
    /// This helper applies no additional redaction or normalization.
    bytes_policy: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    raw_preview: Vec<Preview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Arguments for the same crew executable, not a PATH lookup or command.
    recovery: Option<Vec<String>>,
}

fn show(member: &Member, args: ViewArgs) -> Result<()> {
    let status = CapturedStatus {
        exit_code: args.exit_code,
        signal: args.signal,
        timed_out: args.timed_out,
    };
    let capture = (|| {
        let stdout = read_source(&args.input, MAX_CAPTURE_BYTES)?;
        let stderr = match args.stderr {
            Some(path) => read_source(&path, MAX_CAPTURE_BYTES - stdout.len())?,
            None => Vec::new(),
        };
        Ok::<_, anyhow::Error>(store::Capture {
            status,
            stdout,
            stderr,
        })
    })();
    let capture = match capture {
        Ok(capture) => capture,
        Err(_) => {
            // Even a view failure retains the original outcome as structured
            // data. Do not quote a source path or untrusted file contents.
            let error = serde_json::json!({
                "format_version": output_view::FORMAT_VERSION,
                "kind": "unavailable", "original_status": status,
                "reason": "source_unavailable_or_over_4_mib", "raw_retained": false,
                "source_unchanged": true
            });
            write_json(&error)?;
            bail!("cannot view source; original captured files left unchanged");
        }
    };
    let format = match args.input_format {
        Format::CargoTest => InputFormat::CargoTest,
        Format::Raw => InputFormat::Raw,
        Format::Machine => InputFormat::Machine,
    };
    let stored = member.store().and_then(|store| store.put(&capture));
    // Never omit raw evidence into a summary when recovery could not be saved.
    let mut view = output_view::view(&capture.stdout, &capture.stderr, status, format);
    if stored.is_err() {
        view.view = output_view::View::Raw {
            reason: output_view::RawReason::RecoveryUnavailable,
        };
    }
    let raw = matches!(view.view, output_view::View::Raw { .. });
    let report = Report {
        recovery: stored.as_ref().ok().map(|reference| {
            vec![
                "output".into(),
                "raw".into(),
                reference.clone(),
                "--stream".into(),
                "stdout".into(),
                "--offset".into(),
                "0".into(),
                "--length".into(),
                "65536".into(),
            ]
        }),
        view,
        raw_retained: stored.is_ok(),
        storage_error: stored
            .as_ref()
            .err()
            .map(|_| "raw_store_unavailable; original captured files left unchanged"),
        bytes_policy:
            "unchanged caller-captured bytes; original redaction must precede this helper",
        raw_preview: if raw {
            vec![
                preview(Stream::Stdout, &capture.stdout),
                preview(Stream::Stderr, &capture.stderr),
            ]
        } else {
            Vec::new()
        },
    };
    write_json(&report)?;
    if stored.is_err() {
        bail!("raw store unavailable; no summary produced");
    }
    Ok(())
}

fn write_json(value: &impl Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

fn read_source(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = File::from(crate::os::open_no_symlinks(
        libc::AT_FDCWD,
        path,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        0,
        false,
    )?);
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit as u64 {
        bail!("capture must be a bounded regular file");
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("capture exceeds size limit");
    }
    Ok(bytes)
}

fn recover(member: &Member, args: RawArgs) -> Result<()> {
    let capture = member.store()?.get(&args.reference)?;
    let bytes = match args.stream {
        Stream::Stdout => &capture.stdout,
        Stream::Stderr => &capture.stderr,
    };
    let start = usize::try_from(args.offset).context("byte offset is outside this artifact")?;
    if start > bytes.len() || args.length as usize > MAX_CHUNK_BYTES {
        bail!("byte range is outside this artifact");
    }
    let end = start.saturating_add(args.length as usize).min(bytes.len());
    let metadata = serde_json::json!({
        "format_version": output_view::FORMAT_VERSION, "kind": "raw",
        "reference": args.reference, "original_status": capture.status,
        "stream": args.stream, "start": start, "end": end, "total_bytes": bytes.len(),
        "next_offset": (end < bytes.len()).then_some(end),
        "exact_bytes": true
    });
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    serde_json::to_writer(&mut err, &metadata)?;
    writeln!(err)?;
    std::io::stdout().lock().write_all(&bytes[start..end])?;
    Ok(())
}
