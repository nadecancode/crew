//! Visible connection copy from the approved experience contract. Only safe,
//! typed connector reason codes select a message; raw diagnostics never do.

use crew_client::api::ClientReason;

use super::profile::KeyStorage;

pub const NO_BROWSER_CODES: &str = "Never paste or scan crew web output here. Native setup uses only the host's public profile, checked at the trusted host console.";
pub const PERSISTENCE: &str = "If the host loses its private runtime state, such as after logout or reboot, this profile cannot reconnect automatically. The host operator must provision the endpoint again and approve a new device key.";
pub const FINGERPRINT_HELP: &str =
    "From crew native status at the trusted host console. Do not copy it from the profile above.";
pub const DEVICE_NAME_HELP: &str =
    "This label helps the host operator recognize your request; it does not verify the device.";
pub const REQUEST_ACCESS: &str = "At the trusted host console, open an enrollment window with crew native enroll, then request access here.";
pub const COMPARE_CODE: &str = "Compare all four groups with this request at the trusted host console. Approve access there only if they match.";
pub const MEMORY_ONLY: &str = "Use until this app closes";
pub const MEMORY_CONSEQUENCE: &str = "The device key stays in memory only. When Crew closes, this profile cannot reconnect: the host operator must revoke the old device and approve a new request.";
pub const READ: &str = "Read tasks, messages, questions, transcripts, diffs and output in selected workspaces. This content may include project secrets and host paths.";
pub const CONTROL: &str = "Send messages and answers, change agent behavior, and use agent and team controls. Agents act with their existing host permissions, which may include files outside the selected workspace.";
pub const TERMINAL: &str = "Send commands and keystrokes to an agent's terminal. Commands run with that agent's existing host permissions and may access files outside the selected workspace.";
pub const NO_SANDBOX: &str =
    "Workspace grants limit Crew access. They do not create an OS or filesystem sandbox.";
pub const REMOVE: &str =
    "Removes this profile and retires this app's device key. Host access remains until revoked or expired.";
pub const DISCONNECT: &str = "Disconnect closes this app's connection and keeps the profile and device key. It does not revoke access or stop the team.";
pub const NO_ACCESS: &str = "No workspace access. Ask the host operator to grant this device a workspace with crew native grant at the trusted host console.";
pub const ACCESS_ENDED: &str = "This workspace is no longer available to this device.";
pub const LOST_REPLY: &str = "The connection closed before approval was confirmed here. The host may have approved this device. Reconnect to check, or revoke it at the host.";
pub const REQUEST_CLOSED: &str = "Request closed.";

/// Host-console instructions; the app has no remote administration.
pub fn revoke_instructions(device_id: Option<&str>) -> String {
    let device = device_id.unwrap_or("<device-id>");
    format!(
        "This app cannot change or revoke access. At the trusted host console:\n\
         • crew native devices lists devices and their workspace grants.\n\
         • crew native revoke {device} ends this device's access to every workspace.\n\
         • crew native revoke {device} --workspace <grant> removes one workspace and keeps the others.\n\
         • crew native grant changes Control and Terminal for one workspace."
    )
}

/// Recovery actions offered beside a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    TryAgain,
    EditProfile,
    ReviewProfile,
    ReconnectToCheck,
    ConnectionDetails,
    RemoveProfile,
}

impl Recovery {
    pub fn label(self) -> &'static str {
        match self {
            Self::TryAgain => "Try again",
            Self::EditProfile => "Edit profile",
            Self::ReviewProfile => "Review host profile",
            Self::ReconnectToCheck => "Reconnect to check",
            Self::ConnectionDetails => "Connection details",
            Self::RemoveProfile => "Remove from this app",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub title: &'static str,
    pub message: String,
    pub actions: Vec<Recovery>,
}

/// Where a reason was learned. Enrollment refusals are generic unless the
/// connector names a local trust, key or profile reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connect,
    /// Before the host accepted this attempt's proof.
    Request,
    /// After a verified comparison code was shown.
    Approval,
    /// After authentication, on a live connection.
    Live,
}

pub fn problem(reason: &ClientReason, phase: Phase, storage: KeyStorage) -> Problem {
    let p = |title, message: &str, actions: &[Recovery]| Problem {
        title,
        message: message.to_owned(),
        actions: actions.to_vec(),
    };
    match reason.code.as_str() {
        "invalid_profile" => p(
            "Check the host profile",
            "Check the host profile.",
            &[Recovery::EditProfile],
        ),
        "tls_trust" => p(
            "Host identity could not be verified",
            "Host identity could not be verified. Compare the profile with crew native status at the trusted host console.",
            &[Recovery::ReviewProfile],
        ),
        "key_locked" => p(
            "Credential storage is locked",
            "Unlock this device's credential storage and try again.",
            &[Recovery::TryAgain],
        ),
        "key_unavailable" => p(
            "Secure storage unavailable",
            "Device secure storage is unavailable. Start the desktop keyring (Secret Service), then try again.",
            &[Recovery::TryAgain],
        ),
        "key_canceled" => p(
            "Device access canceled",
            "Unlocking device access was canceled. Nothing was sent to the host.",
            &[Recovery::TryAgain],
        ),
        "key_retired" => match storage {
            KeyStorage::Memory => p(
                "Temporary access ended",
                "This profile's temporary device key ended when Crew closed. Ask the host operator to revoke the old device, then remove this profile and request access again.",
                &[Recovery::RemoveProfile],
            ),
            KeyStorage::SecureStore => p(
                "Device key unavailable",
                "This app no longer has a device key for this profile. Ask the host operator to revoke the old device, then remove this profile and request access again.",
                &[Recovery::RemoveProfile],
            ),
        },
        "setup_busy" => p(
            "Request in progress",
            "Device setup is already in progress.",
            &[],
        ),
        "protocol" => p(
            "Incompatible versions",
            "This app and host use incompatible versions. Check the app and host versions.",
            &[Recovery::ConnectionDetails],
        ),
        "stopped" if phase != Phase::Request => p(
            "Native service is unavailable",
            "Native service is unavailable. Reconnect after the host operator restarts it; stopping the service does not revoke access.",
            &[Recovery::TryAgain],
        ),
        "unreachable" | "closed" | "timeout" if matches!(phase, Phase::Connect | Phase::Live) => p(
            "Cannot reach this host",
            "Cannot reach this host. Check its address and network connection, and ask the host operator to check the native service.",
            &[Recovery::TryAgain, Recovery::EditProfile],
        ),
        "unreachable" if phase == Phase::Request => p(
            "Cannot reach this host",
            "Cannot reach this host. Check its address and network connection, and ask the host operator to check the native service.",
            &[Recovery::TryAgain, Recovery::EditProfile],
        ),
        _ if phase == Phase::Approval => p(
            "Request no longer available",
            "This request is no longer available. Check with the host operator before requesting access again.",
            &[],
        ),
        _ if phase == Phase::Request => p(
            "Access request not accepted",
            "Access request was not accepted. Ask the host operator to open an enrollment window, then try again.",
            &[Recovery::TryAgain],
        ),
        "revoked" if phase == Phase::Live => p(
            "Device access ended",
            "Device access ended. Check with the host operator.",
            &[Recovery::ConnectionDetails],
        ),
        _ => p(
            "Device access unavailable",
            "Device access is unavailable. Check with the host operator.",
            &[Recovery::ConnectionDetails],
        ),
    }
}
