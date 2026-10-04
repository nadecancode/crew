//! `crew web`: the browser dashboard of the session (notes/web-design.md
//! §transport).

use std::io::IsTerminal;

use anyhow::Result;
use crew_core::rpc::{Call, WebInfo};
use serde_json::Value;

use super::Target;

/// Serve the dashboard if it is not served yet, print its address and a
/// one-time pairing address, and open that in the browser.
pub fn web(target: &Target, port: Option<u16>, no_open: bool, json: bool) -> Result<()> {
    // Before anything else: an agent never gets a pairing code.
    crew_core::client::refuse_in_agent_context()?;
    let (mut client, _) = target.connect("cli")?;
    let info: WebInfo = client.call(Call::WebOpen { port })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    println!("web        {}", info.url);
    println!(
        "open       {}   (works once, for {} minutes)",
        info.pair_url,
        info.expires_in_secs / 60
    );
    println!(
        "note       opens in Chromium-based browsers and Firefox; Safari cannot resolve this \
address. Open it yourself, and paste it nowhere else"
    );
    if !no_open && std::io::stdout().is_terminal() {
        open_browser(&info.pair_url);
    }
    Ok(())
}

/// `crew web --stop`: stop serving; every browser session ends.
pub fn web_stop(target: &Target) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let reply: Value = client.call(Call::WebClose)?;
    if reply["stopped"].as_bool() == Some(true) {
        println!("stopped the web dashboard: every browser session has ended");
    } else {
        println!("the web dashboard was not served");
    }
    Ok(())
}

/// Hand `url` to the desktop's opener; failing to is not an error, the
/// address was printed.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let spawned = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
