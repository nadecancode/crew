use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use crew_protocol::native::{HexBytes, LeafHash};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::http::Uri;
use zeroize::Zeroizing;

pub(crate) struct Identity {
    pub config: Arc<rustls::ServerConfig>,
    pub leaf_hash: LeafHash,
    pub trust_pem: String,
    pub host: String,
}

pub(crate) fn validate_endpoint(listen: SocketAddr, endpoint: &str) -> Result<String> {
    if listen.port() == 0
        || listen.ip().is_unspecified()
        || listen.ip().is_multicast()
        || matches!(listen.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
    {
        bail!("native gateway requires an explicit unicast address and nonzero port");
    }
    let uri: Uri = endpoint
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid native wss endpoint"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| anyhow::anyhow!("invalid native wss endpoint"))?;
    if uri.scheme_str() != Some("wss")
        || uri.path() != "/native/ws"
        || uri.query().is_some()
        || authority.as_str().contains('@')
        || authority.port_u16().is_none_or(|port| port == 0)
    {
        bail!("native endpoint must be wss://host:port/native/ws without credentials or query");
    }
    Ok(authority.as_str().to_owned())
}

impl Identity {
    pub fn load(root: &Path, cert_path: &Path, key_path: &Path, host: String) -> Result<Self> {
        // Reject external private paths before reading either private key or binding.
        let key_bytes = Zeroizing::new(super::store::read_tls_key(root, key_path)?);
        let cert_bytes = std::fs::read(cert_path).context("reading native TLS certificate")?;
        if cert_bytes.is_empty() || cert_bytes.len() > 64 * 1024 {
            bail!("native TLS certificate exceeds its limit");
        }
        let certs = rustls_pemfile::certs(&mut BufReader::new(cert_bytes.as_slice()))
            .collect::<std::io::Result<Vec<_>>>()?;
        if certs.is_empty() || certs.len() > 8 {
            bail!("native TLS certificate chain is invalid");
        }
        let key = rustls_pemfile::private_key(&mut BufReader::new(key_bytes.as_slice()))?
            .ok_or_else(|| anyhow::anyhow!("native TLS private key is invalid"))?;
        let leaf_hash = HexBytes(Sha256::digest(certs[0].as_ref()).into());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
        config.max_early_data_size = 0;
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(Self {
            config: Arc::new(config),
            leaf_hash,
            trust_pem: String::from_utf8(cert_bytes)
                .map_err(|_| anyhow::anyhow!("native TLS certificate must be PEM"))?,
            host,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_is_explicit_unicast_wss_without_hidden_credentials_or_routes() {
        let addr = "127.0.0.1:12345".parse().unwrap();
        assert_eq!(
            validate_endpoint(addr, "wss://127.0.0.1:12345/native/ws").unwrap(),
            "127.0.0.1:12345"
        );
        for url in [
            "ws://host:123/native/ws",
            "wss://host/native/ws",
            "wss://u:p@host:123/native/ws",
            "wss://host:123/native/ws?x=1",
            "wss://host:123/",
        ] {
            assert!(validate_endpoint(addr, url).is_err());
        }
        assert!(validate_endpoint(
            "0.0.0.0:12345".parse().unwrap(),
            "wss://host:12345/native/ws"
        )
        .is_err());
    }
}
