//! Local-only native administration. Agent context is refused before effects.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum NativeCommand {
    /// Start the optional native gateway at an explicit TLS endpoint
    Start {
        #[arg(long)]
        listen: SocketAddr,
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        tls_cert: PathBuf,
        #[arg(long)]
        tls_key: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, hide = true)]
        foreground: bool,
    },
    /// Print the public host profile and TLS fingerprint; grants stay local
    Status,
    /// Print private runtime locations before provisioning the host TLS key
    Paths,
    /// Open a 120-second host-approved device enrollment window
    Enroll,
    /// List pending device intents and comparison codes on this console
    Pending,
    /// Approve an intended device with explicit selected workspaces
    Approve {
        enrollment_id: String,
        #[arg(long)]
        code: String,
        #[arg(long)]
        key_hash: String,
        #[arg(long, required = true)]
        workspace: Vec<PathBuf>,
        /// Allow messages/lifecycle actions that steer agents with their existing host privileges
        #[arg(long)]
        control: bool,
        /// Allow agent command input; this does not create an OS/filesystem sandbox
        #[arg(long)]
        terminal: bool,
        #[arg(long, default_value_t = 2592000)]
        lifetime_secs: u64,
    },
    /// List public device records and selected grants on this console
    Devices,
    /// Set one selected workspace grant; Control and Terminal remain explicit
    Grant {
        device_id: String,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        control: bool,
        #[arg(long)]
        terminal: bool,
    },
    /// Revoke a device or one opaque workspace grant and drain live channels
    Revoke {
        device_id: String,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Stop only the native gateway; device records remain until runtime-state loss
    Stop,
}

pub fn native(command: NativeCommand) -> Result<()> {
    crew_core::client::refuse_in_agent_context()?;
    #[cfg(not(feature = "native-gateway"))]
    {
        let _ = command;
        bail!("not built with the gateway; build crew with --features native-gateway");
    }
    #[cfg(feature = "native-gateway")]
    run(command)
}

#[cfg(feature = "native-gateway")]
fn run(command: NativeCommand) -> Result<()> {
    use serde_json::json;
    match command {
        NativeCommand::Paths => {
            let runtime = crew_core::paths::runtime_dir();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"runtime_dir":runtime,"native_dir":runtime.join("native")})
                )?
            );
            Ok(())
        }
        NativeCommand::Start {
            listen,
            endpoint,
            tls_cert,
            tls_key,
            name,
            foreground,
        } => {
            let config = crate::native::Config {
                listen,
                endpoint: endpoint.clone(),
                certificate: tls_cert.clone(),
                private_key: tls_key.clone(),
                name: name.clone(),
                runtime: crew_core::paths::runtime_dir(),
            };
            if foreground {
                return crate::native::foreground(config);
            }
            crate::native::validate_config(&config)?;
            if crate::native::admin_call("native_status", json!({})).is_ok() {
                bail!("native gateway already running");
            }
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .args([
                    "native",
                    "start",
                    "--foreground",
                    "--listen",
                    &listen.to_string(),
                    "--endpoint",
                    &endpoint,
                    "--name",
                    &name,
                ])
                .arg("--tls-cert")
                .arg(tls_cert)
                .arg("--tls-key")
                .arg(tls_key)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: setsid is async-signal-safe and runs in the child before exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn()?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Ok(status) = crate::native::admin_call("native_status", json!({})) {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                    return Ok(());
                }
                if child.try_wait()?.is_some() {
                    bail!("native gateway did not start");
                }
                if std::time::Instant::now() >= deadline {
                    bail!("native gateway did not answer within 5 seconds");
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        command => {
            let (method, params) = match command {
                NativeCommand::Status => ("native_status", json!({})),
                NativeCommand::Enroll => ("native_enroll", json!({})),
                NativeCommand::Pending => ("native_pending", json!({})),
                NativeCommand::Devices => ("native_devices", json!({})),
                NativeCommand::Approve {
                    enrollment_id,
                    code,
                    key_hash,
                    workspace,
                    control,
                    terminal,
                    lifetime_secs,
                } => {
                    grant_notice(control, terminal);
                    (
                        "native_approve",
                        json!({"enrollment_id":enrollment_id,"comparison_code":code.split_whitespace().collect::<String>(),"public_key_hash":key_hash,"grants":workspace.into_iter().map(|workspace|json!({"workspace":workspace,"control":control,"terminal":terminal})).collect::<Vec<_>>(),"lifetime_secs":lifetime_secs}),
                    )
                }
                NativeCommand::Grant {
                    device_id,
                    workspace,
                    control,
                    terminal,
                } => {
                    grant_notice(control, terminal);
                    (
                        "native_grant",
                        json!({"device_id":device_id,"selection":{"workspace":workspace,"control":control,"terminal":terminal}}),
                    )
                }
                NativeCommand::Revoke {
                    device_id,
                    workspace,
                } => (
                    "native_revoke",
                    json!({"device_id":device_id,"workspace":workspace}),
                ),
                NativeCommand::Stop => ("native_stop", json!({})),
                NativeCommand::Start { .. } => unreachable!(),
                NativeCommand::Paths => unreachable!(),
            };
            let result = crate::native::admin_call(method, params)?;
            if method == "native_stop" {
                let socket = crew_core::paths::runtime_dir().join("native.sock");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while socket.exists() {
                    if std::time::Instant::now() >= deadline {
                        bail!("native gateway did not release its control socket");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
            println!("{}", serde_json::to_string_pretty(&result)?);
            if method == "native_status" {
                let fingerprint = result
                    .pointer("/profile/tls_leaf_sha256")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|hex| crew_protocol::native::LeafHash::from_hex(hex).ok());
                if let Some(fingerprint) = fingerprint {
                    let hex = fingerprint.to_hex();
                    let groups = hex
                        .as_bytes()
                        .chunks(4)
                        .map(String::from_utf8_lossy)
                        .collect::<Vec<_>>()
                        .join(" ");
                    eprintln!("Fingerprint: {groups}");
                }
            }
            Ok(())
        }
    }
}

#[cfg(feature = "native-gateway")]
fn grant_notice(control: bool, terminal: bool) {
    if control || terminal {
        eprintln!("Control and Terminal can steer agents with their existing host privileges. Workspace grants limit Crew resources, not filesystem or OS access.");
    }
}
