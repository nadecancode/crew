//! Connection lifecycle copy for the status line and the workspace selector.
//! The connector's own reason, when it gives one, goes on a second line.

use crew_client::api::{EndpointStatus, WorkspaceStatus};

/// One phrase for the current endpoint and workspace state.
pub fn phrase(endpoint: EndpointStatus, workspace: WorkspaceStatus) -> &'static str {
    match endpoint {
        EndpointStatus::Authenticated => match workspace {
            WorkspaceStatus::NoneSelected => "Connected · choose a workspace",
            WorkspaceStatus::Connecting => "Synchronizing team",
            WorkspaceStatus::Live => "Live",
            WorkspaceStatus::Reconnecting => "Reconnecting · last known data",
            WorkspaceStatus::Stopped => "Team is not running",
            WorkspaceStatus::Unreachable => "Team cannot be reached",
            WorkspaceStatus::Incompatible => "This app and host use incompatible versions",
            WorkspaceStatus::Removed | WorkspaceStatus::AccessRevoked => {
                "This workspace is no longer available to this device"
            }
            WorkspaceStatus::NeverJoined => "Not joined · choose a workspace",
        },
        EndpointStatus::Disconnected => "Not connected",
        EndpointStatus::Connecting => "Connecting",
        EndpointStatus::Reconnecting => "Reconnecting · last known data",
        EndpointStatus::SignedOut => "Signed out",
        EndpointStatus::Incompatible => "Incompatible versions · check the app and host versions",
        EndpointStatus::Stopped => "Team is not running",
    }
}

/// The phrase, then the connector's reason on its own line.
pub fn status(
    endpoint: EndpointStatus,
    workspace: WorkspaceStatus,
    reason: Option<&str>,
) -> String {
    let phrase = phrase(endpoint, workspace);
    match reason.filter(|reason| !reason.is_empty()) {
        Some(reason) => format!("{phrase}\n{reason}"),
        None => phrase.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_reads_as_one_phrase_without_debug_names() {
        let endpoints = [
            EndpointStatus::Disconnected,
            EndpointStatus::Connecting,
            EndpointStatus::Authenticated,
            EndpointStatus::Reconnecting,
            EndpointStatus::SignedOut,
            EndpointStatus::Incompatible,
            EndpointStatus::Stopped,
        ];
        let workspaces = [
            WorkspaceStatus::NoneSelected,
            WorkspaceStatus::Connecting,
            WorkspaceStatus::Live,
            WorkspaceStatus::Reconnecting,
            WorkspaceStatus::Stopped,
            WorkspaceStatus::Unreachable,
            WorkspaceStatus::Incompatible,
            WorkspaceStatus::Removed,
            WorkspaceStatus::NeverJoined,
            WorkspaceStatus::AccessRevoked,
        ];
        for endpoint in endpoints {
            for workspace in workspaces {
                let text = phrase(endpoint, workspace);
                assert!(!text.contains('\n'), "{text}");
                // No `{:?}` names such as NoneSelected or Authenticated.
                assert!(
                    text.split_whitespace()
                        .all(|word| !word.chars().skip(1).any(char::is_uppercase)),
                    "{text}"
                );
                assert_ne!(text, "Authenticated", "{endpoint:?} {workspace:?}");
            }
        }
        assert_eq!(
            phrase(EndpointStatus::Authenticated, WorkspaceStatus::Live),
            "Live"
        );
        assert_eq!(
            phrase(EndpointStatus::Disconnected, WorkspaceStatus::NoneSelected),
            "Not connected"
        );
        assert_eq!(
            status(
                EndpointStatus::Reconnecting,
                WorkspaceStatus::Live,
                Some("The daemon closed the connection.")
            ),
            "Reconnecting · last known data\nThe daemon closed the connection."
        );
    }
}
