//! Opt-in authenticated native gateway. The browser dashboard stays local.

pub(crate) mod admin;
pub(crate) mod auth;
pub(crate) mod control;
pub(crate) mod http;
pub(crate) mod limits;
pub(crate) mod members;
pub(crate) mod policy;
pub(crate) mod route;
pub(crate) mod run;
pub(crate) mod service;
pub(crate) mod store;
pub(crate) mod tls;
pub(crate) mod ws;

pub use admin::admin_call;
pub use run::foreground;

pub fn validate_config(config: &Config) -> anyhow::Result<()> {
    crew_core::client::refuse_in_agent_context()?;
    let host = tls::validate_endpoint(config.listen, &config.endpoint)?;
    if !crew_protocol::native::valid_label(&config.name) {
        anyhow::bail!("invalid native endpoint name");
    }
    let root = config.runtime.join("native");
    let _ = tls::Identity::load(&root, &config.certificate, &config.private_key, host)?;
    Ok(())
}
pub use service::{Config, Gateway};

use crew_protocol::native::HexBytes;

pub(crate) fn fresh<const N: usize>() -> anyhow::Result<HexBytes<N>> {
    let mut bytes = [0; N];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("native cryptographic entropy unavailable"))?;
    Ok(HexBytes(bytes))
}

pub(crate) fn now_ms() -> i64 {
    crew_core::util::now_ms()
}
