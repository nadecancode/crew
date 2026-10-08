//! Codex discovery controls that require the native CLI or app-server.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};

/// Run discovery with the same non-MCP overrides as the eventual session.
/// Inventory must precede injecting our servers: a same-name ambient URL,
/// environment or tool filter would otherwise survive Codex's deep merge.
pub(crate) fn inventory_args(args: &[String], include_mcp: bool) -> Vec<String> {
    let mut result = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "--config" {
            if let Some(value) = args.next() {
                if include_mcp
                    || !(value.starts_with("mcp_servers.") || value.starts_with("mcp_servers="))
                {
                    result.extend(["-c".to_string(), value.clone()]);
                }
            }
        } else if arg == "--profile" || arg == "-p" {
            if let Some(value) = args.next() {
                result.extend(["--profile".to_string(), value.clone()]);
            }
        } else if let Some(value) = arg.strip_prefix("--config=") {
            result.extend(["-c".to_string(), value.to_string()]);
        } else if arg.starts_with("--profile=") {
            result.push(arg.clone());
        }
    }
    result.extend(["mcp".into(), "list".into(), "--json".into()]);
    result
}

/// Never retain native transport objects: they can contain operator secrets.
pub(crate) fn inventory_names(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let items: Value =
        serde_json::from_slice(bytes).context("Codex MCP inventory returned no valid JSON")?;
    let items = items
        .as_array()
        .ok_or_else(|| anyhow!("Codex MCP inventory returned no server array"))?;
    items
        .iter()
        .map(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty() && name.len() <= 1024)
                .map(str::to_string)
                .ok_or_else(|| anyhow!("Codex MCP inventory returned an invalid server name"))
        })
        .collect()
}

/// Set server-wide roots before the TUI connects. Native Codex 0.159.0
/// captures confirm that roots survive this setup connection closing and
/// are included in the subsequently connected TUI's provider request.
pub(crate) async fn set_skill_roots(socket: &Path, roots: &[PathBuf]) -> Result<()> {
    if roots.is_empty() {
        return Ok(());
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let stream = UnixStream::connect(socket).await?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        let (mut connection, _) = tokio_tungstenite::client_async_with_config(
            "ws://localhost/",
            stream,
            Some(config),
        )
        .await?;
        for (id, method, params) in [
            (
                1,
                "initialize",
                json!({"clientInfo":{"name":"crew-toolbox","version":env!("CARGO_PKG_VERSION"),"title":"Crew"},"capabilities":{"experimentalApi":true,"requestAttestation":false}}),
            ),
            (2, "skills/extraRoots/set", json!({"extraRoots":roots})),
        ] {
            connection
                .send(Message::Text(json!({"id":id,"method":method,"params":params}).to_string().into()))
                .await?;
            loop {
                let message = connection
                    .next()
                    .await
                    .ok_or_else(|| anyhow!("Codex closed toolbox setup before acknowledging {method}"))??;
                let Message::Text(text) = message else { continue };
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id") != Some(&json!(id)) {
                    continue;
                }
                // An upstream error may contain configuration or credentials.
                // Only the operation, never its payload, reaches diagnostics.
                if value.get("error").is_some() || !value.get("result").is_some_and(Value::is_object) {
                    bail!("Codex did not acknowledge toolbox {method}");
                }
                break;
            }
            if id == 1 {
                connection.send(Message::Text(json!({"method":"initialized"}).to_string().into())).await?;
            }
        }
        // Configuration is committed once acknowledged. A peer closing the
        // setup connection immediately afterwards does not undo that result.
        let _ = connection.close(None).await;
        Ok(())
    })
    .await
    .map_err(|_| anyhow!("Codex toolbox skill setup did not finish within 15 seconds"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_uses_config_without_injected_servers_or_session_commands() {
        let args = [
            "--remote",
            "unix:///fixture",
            "-c",
            "mcp_servers.crew.command=\"crew\"",
            "-c",
            "features.hooks=false",
            "--profile",
            "worker",
            "resume",
            "session",
        ]
        .map(str::to_string);
        assert_eq!(
            inventory_args(&args, false),
            [
                "-c",
                "features.hooks=false",
                "--profile",
                "worker",
                "mcp",
                "list",
                "--json"
            ]
        );
        assert_eq!(inventory_names(br#"[{"name":"z","transport":{"env":{"SECRET":"sentinel"}}},{"name":"a","enabled":false}]"#).unwrap().into_iter().collect::<Vec<_>>(), ["a", "z"]);
        assert!(inventory_names(br#"{"error":"sentinel"}"#).is_err());
    }

    #[tokio::test]
    async fn roots_wait_for_acknowledgement_and_reject_native_errors() {
        for reject in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("app.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut requests = Vec::new();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    requests.push(request.clone());
                    if let Some(id) = request.get("id") {
                        let response = if reject && id == 2 {
                            json!({"id":id,"error":{"message":"SECRET"}})
                        } else {
                            json!({"id":id,"result":{}})
                        };
                        ws.send(Message::Text(response.to_string().into()))
                            .await
                            .unwrap();
                        if id == 2 {
                            break;
                        }
                    }
                }
                requests
            });
            let result = set_skill_roots(&socket, &[PathBuf::from("/snapshot/skill")]).await;
            if reject {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("skills/extraRoots/set"));
                assert!(!error.contains("SECRET"));
            } else {
                result.unwrap();
            }
            let requests = server.await.unwrap();
            assert_eq!(requests[1]["method"], "initialized");
            assert_eq!(
                requests[2]["params"]["extraRoots"],
                json!(["/snapshot/skill"])
            );
        }
    }
}
