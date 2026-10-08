//! Opt-in skills embedded in crew. Runtime paths are bindings, not skill bytes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crew_core::output_view::{FORMAT_VERSION, PARSER_ID};
use serde::{Deserialize, Serialize};

use super::store::{BTreeFiles, Snapshot, Store};

pub const HELPER_ENV: &str = "CREW_COMPACT_CLI_HELPER";
const SKILL: &[u8] = include_bytes!("../../../assets/compact-cli/SKILL.md");

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    helper_sha256: String,
    format_version: u32,
    parser: String,
}

pub fn snapshot(
    store: &Store,
    pin: Option<&Path>,
    inherited: Option<&Snapshot>,
) -> Result<(Snapshot, PathBuf)> {
    let pin = pin.context("compact-cli needs a verified daemon executable pin")?;
    let hash = crate::output::binary::pinned_digest(pin)?;
    if let Some(snapshot) = inherited {
        let files = store.files(snapshot)?;
        let manifest: Manifest = serde_json::from_slice(
            &files
                .get(Path::new("helper.json"))
                .context("compact-cli helper manifest missing")?
                .0,
        )?;
        if manifest.helper_sha256 != hash
            || manifest.format_version != FORMAT_VERSION
            || manifest.parser != PARSER_ID
        {
            bail!("compact-cli's inherited helper is unavailable in this build; recreate this subagent from a restarted parent");
        }
        return Ok((snapshot.clone(), pin.to_path_buf()));
    }
    let manifest = Manifest {
        helper_sha256: hash.clone(),
        format_version: FORMAT_VERSION,
        parser: PARSER_ID.into(),
    };
    // Only delegate explicit view/recovery arguments. Neither a producer command
    // nor a shell expression is evaluated here. The helper verifies its own inode.
    let script = format!("#!/bin/sh\nset -eu\n: \"${{{HELPER_ENV}:?compact-cli helper unavailable; restart or reload the toolbox}}\"\nexec \"${HELPER_ENV}\" output --expected-helper '{hash}' \"$@\"\n");
    let files = BTreeFiles::from([
        ("SKILL.md".into(), (SKILL.to_vec(), false)),
        (
            "helper.json".into(),
            (serde_json::to_vec_pretty(&manifest)?, false),
        ),
        ("scripts/output".into(), (script.into_bytes(), true)),
    ]);
    Ok((store.snapshot_files(&files)?, pin.to_path_buf()))
}
