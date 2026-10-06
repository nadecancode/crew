use std::sync::Arc;

use crew_protocol::native::{LeafHash, PublicProfile};
use tokio_tungstenite::tungstenite::http::Uri;

use super::GatewayError;

/// Public imported metadata becomes trusted only after an independent
/// fingerprint comparison. Its private fields prevent a connector from being
/// constructed with an unconfirmed pin or changed URL.
#[derive(Clone)]
pub struct GatewayProfile {
    pub(super) id: String,
    pub(super) public: PublicProfile,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) tls: Arc<rustls::ClientConfig>,
}

impl GatewayProfile {
    pub fn trust(
        id: String,
        profile: PublicProfile,
        independently_verified_fingerprint: LeafHash,
    ) -> Result<Self, GatewayError> {
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(char::is_control)
            || !crew_protocol::native::valid_label(&profile.name)
            || profile.url.len() > 4096
            || profile.trust_pem.is_empty()
            || profile.trust_pem.len() > 64 * 1024
        {
            return Err(GatewayError::InvalidProfile);
        }
        if profile.tls_leaf_sha256 != independently_verified_fingerprint {
            return Err(GatewayError::Trust);
        }
        let uri: Uri = profile
            .url
            .parse()
            .map_err(|_| GatewayError::InvalidProfile)?;
        let authority = uri.authority().ok_or(GatewayError::InvalidProfile)?;
        if uri.scheme_str() != Some("wss")
            || uri.path() != "/native/ws"
            || uri.query().is_some()
            || profile.url.contains('#')
            || authority.as_str().contains('@')
            || authority.port_u16().is_none_or(|port| port == 0)
        {
            return Err(GatewayError::InvalidProfile);
        }
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let port = authority.port_u16().ok_or(GatewayError::InvalidProfile)?;
        rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| GatewayError::InvalidProfile)?;
        let mut roots = rustls::RootCertStore::empty();
        let certs =
            rustls_pemfile::certs(&mut std::io::BufReader::new(profile.trust_pem.as_bytes()))
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|_| GatewayError::Trust)?;
        if certs.is_empty() || certs.len() > 8 {
            return Err(GatewayError::Trust);
        }
        for cert in certs {
            roots.add(cert).map_err(|_| GatewayError::Trust)?;
        }
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| GatewayError::Trust)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        tls.enable_early_data = false;
        tls.resumption = rustls::client::Resumption::disabled();
        Ok(Self {
            id,
            public: profile,
            host,
            port,
            tls: Arc::new(tls),
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.id
    }

    pub fn endpoint_id(&self) -> String {
        self.public.endpoint_id.to_hex()
    }

    /// Contains public trust material, never a credential or device-store handle.
    pub fn public_profile(&self) -> &PublicProfile {
        &self.public
    }
}

impl std::fmt::Debug for GatewayProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GatewayProfile(<public trusted metadata>)")
    }
}
