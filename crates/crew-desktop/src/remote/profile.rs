//! Remote connection profiles. Only public host metadata and presentation
//! fields persist; device keys stay in the OS credential service (or in memory
//! after an explicit temporary choice). Nothing here is a credential.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crew_client_transport::gateway::{GatewayError, GatewayProfile};
use crew_protocol::native::{self, HexBytes, LeafHash, PublicProfile};
use serde::{Deserialize, Serialize};

/// Where this profile's device signing key lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStorage {
    /// The user's Secret Service collection.
    SecureStore,
    /// Process memory only; the key ends when Crew closes.
    Memory,
}

/// Non-secret facts from the last authenticated enrollment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedAccess {
    pub device_id: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProfile {
    /// Local identifier; never a host or device identity.
    pub id: String,
    /// The user's local description of this connection.
    pub label: String,
    pub public: PublicProfile,
    /// The fingerprint the operator entered from `crew native status` at the
    /// trusted host console. The connector compares it with the profile pin.
    pub entered_fingerprint: LeafHash,
    pub device_label: Option<String>,
    pub storage: KeyStorage,
    pub saved_access: Option<SavedAccess>,
}

impl RemoteProfile {
    /// The connector's own profile and trust checks, with the entered value.
    pub fn trusted(&self) -> Result<GatewayProfile, GatewayError> {
        GatewayProfile::trust(
            self.id.clone(),
            self.public.clone(),
            self.entered_fingerprint,
        )
    }

    pub fn endpoint_id(&self) -> String {
        self.public.endpoint_id.to_hex()
    }
}

/// The inputs named on the host profile page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Label,
    Profile,
    Fingerprint,
    DeviceName,
}

impl Field {
    pub fn title(self) -> &'static str {
        match self {
            Self::Label => "Connection name",
            Self::Profile => "Public host profile",
            Self::Fingerprint => "Host fingerprint",
            Self::DeviceName => "Device name",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

fn error(field: Field, message: impl Into<String>) -> FieldError {
    FieldError {
        field,
        message: message.into(),
    }
}

/// Raw, non-secret text from the host profile form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileInput {
    pub label: String,
    /// `crew native status` output or a bare public profile.
    pub profile: String,
    /// Typed by the operator from the trusted host console; never prefilled.
    pub fingerprint: String,
}

/// A checked form, ready for the Verify this host comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedInput {
    pub label: String,
    pub public: PublicProfile,
    pub entered: LeafHash,
}

/// The shared fingerprint rule (also `crew_client_ffi::normalizeFingerprint`):
/// ASCII whitespace and colons are ignored, letters are lowercased, and the
/// result must be exactly 64 characters of `0-9a-f`.
pub fn normalize_fingerprint(text: &str) -> Option<String> {
    let value = text
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect::<String>();
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

/// Characters the fingerprint field would keep, for its live count/echo.
pub fn fingerprint_characters(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Accepts `crew native status` output or a bare public profile. Unknown
/// fields are refused, so a secret-bearing document is never half-read.
pub fn parse_public_profile(text: &str) -> Option<PublicProfile> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Status {
        profile: PublicProfile,
        #[allow(dead_code)]
        endpoint_epoch: Option<serde_json::Value>,
    }
    if text.len() > 128 * 1024 {
        return None;
    }
    serde_json::from_str::<Status>(text)
        .map(|status| status.profile)
        .or_else(|_| serde_json::from_str::<PublicProfile>(text))
        .ok()
}

impl ProfileInput {
    /// Field checks in reading order. They construct nothing trusted: only
    /// `RemoteProfile::trusted`, with the entered fingerprint, does that.
    pub fn check(&self) -> Result<CheckedInput, FieldError> {
        let label = self.label.trim();
        if !label.is_empty() && !native::valid_label(label) {
            return Err(error(
                Field::Label,
                "Enter a short name for this connection, up to 64 characters.",
            ));
        }
        const PROFILE: &str =
            "Check the host profile. Paste the output of crew native status from the host.";
        let public = parse_public_profile(self.profile.trim())
            .filter(public_shape)
            .ok_or(error(Field::Profile, PROFILE))?;
        let entered = normalize_fingerprint(&self.fingerprint)
            .and_then(|hex| LeafHash::from_hex(&hex).ok())
            .ok_or_else(|| {
                error(
                    Field::Fingerprint,
                    format!(
                        "The fingerprint has 64 hexadecimal characters (0–9, a–f). This one has {}.",
                        fingerprint_characters(&self.fingerprint).chars().count()
                    ),
                )
            })?;
        Ok(CheckedInput {
            // An empty name defaults to the host's published name.
            label: if label.is_empty() {
                public.name.clone()
            } else {
                label.to_owned()
            },
            public,
            entered,
        })
    }
}

/// Public-field shape only, as the connector will check it again: a wss
/// `/native/ws` endpoint with an explicit port and no query, fragment or
/// user information, a valid host name and bounded PEM trust material.
fn public_shape(public: &PublicProfile) -> bool {
    native::valid_label(&public.name)
        && public.url.len() <= 4096
        && endpoint_shape(&public.url)
        && !public.trust_pem.is_empty()
        && public.trust_pem.len() <= 64 * 1024
        && public.trust_pem.contains("-----BEGIN CERTIFICATE-----")
        && public.trust_pem.contains("-----END CERTIFICATE-----")
}

fn endpoint_shape(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("wss://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    path == "native/ws"
        && !authority.is_empty()
        && !authority.contains('@')
        && !url.contains(['?', '#'])
        && authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .is_some_and(|port| port != 0)
}

/// Hex grouped for reading, e.g. fingerprints in fours. Lower case, as
/// `crew native status` prints it, so the operator compares equal strings.
pub fn group_hex(hex: &str, size: usize) -> String {
    groups(hex, size).join(" ")
}

pub fn groups(hex: &str, size: usize) -> Vec<String> {
    hex.as_bytes()
        .chunks(size)
        .map(|chunk| String::from_utf8_lossy(chunk).to_lowercase())
        .collect()
}

pub fn valid_device_name(name: &str) -> Result<String, FieldError> {
    let name = name.trim();
    if native::valid_label(name) {
        Ok(name.to_owned())
    } else {
        Err(error(
            Field::DeviceName,
            "Enter a device name up to 64 characters, such as Work laptop.",
        ))
    }
}

/// A fresh local profile identifier. It carries no host or device identity.
pub fn new_profile_id() -> Result<String, &'static str> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| "Random numbers are unavailable.")?;
    Ok(format!("remote-{}", HexBytes(bytes).to_hex()))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreFile {
    version: u32,
    profiles: Vec<RemoteProfile>,
}

/// Public profile list in the user's configuration directory, written with
/// owner-only permissions and atomic replacement.
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn user() -> Self {
        Self::new(
            gtk::glib::user_config_dir()
                .join("crew-desktop")
                .join("connections.json"),
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<RemoteProfile>, &'static str> {
        const UNREADABLE: &str = "Saved connections could not be read. They were left unchanged.";
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes)
                .ok()
                .filter(|file| file.version == 1)
                .map(|file| file.profiles)
                .ok_or(UNREADABLE),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(UNREADABLE),
        }
    }

    pub fn save(&self, profiles: &[RemoteProfile]) -> Result<(), &'static str> {
        const FAILED: &str = "Connections could not be saved.";
        let directory = self.path.parent().ok_or(FAILED)?;
        std::fs::create_dir_all(directory).map_err(|_| FAILED)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| FAILED)?;
        let bytes = serde_json::to_vec_pretty(&StoreFile {
            version: 1,
            profiles: profiles.to_vec(),
        })
        .map_err(|_| FAILED)?;
        let temporary = self.path.with_extension("json.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| FAILED)?;
        file.write_all(&bytes).map_err(|_| FAILED)?;
        file.sync_all().map_err(|_| FAILED)?;
        std::fs::rename(&temporary, &self.path).map_err(|_| FAILED)
    }
}
