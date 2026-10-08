//! Prevent raw CLI arguments from overriding the resolved discovery policy.

use anyhow::{bail, Result};
use crew_core::RuntimeKind;

pub(crate) fn validate(runtime: RuntimeKind, args: &[String], path: &str) -> Result<()> {
    for (index, arg) in args.iter().enumerate() {
        let flag = arg.split('=').next().unwrap_or(arg);
        let replacement = match runtime {
            RuntimeKind::Claude => match flag {
                "--mcp-config" | "--strict-mcp-config" => Some("toolbox.mcp / toolbox.inherit"),
                "--setting-sources" | "--settings" | "--add-dir" | "--disable-slash-commands" => {
                    Some("toolbox.inherit")
                }
                "--plugin-dir" | "--plugin-url" => Some("toolbox.plugins"),
                "--agents" => Some("toolbox.subagents"),
                "--system-prompt-snapshot" => {
                    Some("toolbox (prompt snapshot policy is managed by crew)")
                }
                _ => None,
            },
            RuntimeKind::Pi => match flag {
                "-e" | "--extension" | "--no-extensions" => {
                    Some("toolbox.plugins / toolbox.inherit")
                }
                "--skill" | "--no-skills" => Some("toolbox.skills / toolbox.inherit"),
                "--no-mcp" | "--approve" | "--no-approve" => Some("toolbox.mcp / toolbox.inherit"),
                "--system-prompt" | "--append-system-prompt" | "--no-context-files" => {
                    Some("toolbox.inherit")
                }
                _ if arg.starts_with("-e") && !arg.starts_with("--") && arg.len() > 2 => {
                    Some("toolbox.inherit")
                }
                _ => None,
            },
            RuntimeKind::Codex => {
                let config = if flag == "--config" || arg == "-c" {
                    arg.split_once('=')
                        .map(|(_, value)| value)
                        .or_else(|| args.get(index + 1).map(String::as_str))
                } else {
                    arg.strip_prefix("-c").filter(|_| !arg.starts_with("--"))
                };
                if config.is_some_and(reserved_config) {
                    Some("toolbox")
                } else if flag == "--profile" || (arg.starts_with("-p") && !arg.starts_with("--")) {
                    Some("toolbox.inherit (profiles combine discovery sources)")
                } else if matches!(flag, "--enable" | "--disable") {
                    let value = arg
                        .split_once('=')
                        .map(|(_, value)| value)
                        .or_else(|| args.get(index + 1).map(String::as_str));
                    value
                        .filter(|value| {
                            value.split(',').any(|name| {
                                matches!(name.trim(), "hooks" | "multi_agent" | "plugins")
                            })
                        })
                        .map(|_| "toolbox.inherit")
                } else {
                    None
                }
            }
            RuntimeKind::Shell => None,
        };
        if let Some(replacement) = replacement {
            // Only the flag is displayed. The value might be a credential.
            bail!("{path}[{index}]: {flag} overrides toolbox discovery; use {replacement}");
        }
    }
    Ok(())
}

fn reserved_config(value: &str) -> bool {
    let Some((key, _)) = value.split_once('=') else {
        return false;
    };
    // Parse the key alone so quoted/dotted keys cannot bypass the guard.
    let Ok(table) = format!("{key}=0").parse::<toml::Table>() else {
        return false;
    };
    if [
        "mcp_servers",
        "skills",
        "hooks",
        "agents",
        "plugins",
        "project_doc_max_bytes",
        "developer_instructions",
    ]
    .iter()
    .any(|key| table.contains_key(*key))
    {
        return true;
    }
    table.get("features").is_some_and(|value| {
        value.as_table().is_none_or(|features| {
            ["hooks", "multi_agent", "plugins"]
                .iter()
                .any(|key| features.contains_key(*key))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_controls_cannot_be_reintroduced_through_raw_arguments() {
        for args in [
            vec!["-c", "mcp_servers.foo.command='SECRET'"],
            vec!["--config=\"skills\".config=[]"],
            vec!["-cfeatures={hooks=true}"],
            vec!["--enable", "hooks"],
        ] {
            let error = validate(
                RuntimeKind::Codex,
                &args.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "agents[0].extra_args",
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("agents[0].extra_args"));
            assert!(!error.contains("SECRET"));
        }
        validate(
            RuntimeKind::Codex,
            &[
                "-c".into(),
                "model_reasoning_effort='high'".into(),
                "--no-alt-screen".into(),
            ],
            "extra_args",
        )
        .unwrap();
        assert!(validate(
            RuntimeKind::Claude,
            &["--plugin-dir=/secret".into()],
            "extra_args"
        )
        .is_err());
        for args in [
            vec!["--system-prompt-snapshot", "on"],
            vec!["--system-prompt-snapshot=off"],
        ] {
            assert!(validate(
                RuntimeKind::Claude,
                &args.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "extra_args"
            )
            .is_err());
        }
        assert!(validate(RuntimeKind::Pi, &["-e/some/extension".into()], "extra_args").is_err());
        validate(
            RuntimeKind::Shell,
            &["--skill=anything".into()],
            "extra_args",
        )
        .unwrap();
    }
}
