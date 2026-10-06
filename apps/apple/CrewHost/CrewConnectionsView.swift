import CrewBindings
import CrewUI
import SwiftUI

/// Local profiles come from CrewClient.local's protected discovery snapshot.
/// Selecting an opaque profile calls the real facade; no registry, socket path,
/// daemon identity or workspace is manufactured in Swift.
@MainActor
struct CrewConnectionsView: View {
    @Bindable var model: CrewSessionModel

    var body: some View {
        List {
            if let error = model.error {
                Section("Connection issue") { Text(verbatim: error).textSelection(.enabled) }
            }
            #if os(macOS)
            Section("On this Mac") {
                Text("Local operator access uses this Mac's protected Crew connection. It does not start a team or expose a server.")
                    .font(.callout)
                if model.discovering { ProgressView("Discovering local profiles") }
                ForEach(model.localProfiles, id: \.profileId) { profile in
                    Button { model.connect(profile: profile) } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: profile.project ?? "Local Crew workspace").font(.headline)
                            Text(verbatim: profile.workspace).font(.body.monospaced()).textSelection(.enabled)
                            Text("Connect using local operator authority").font(.callout)
                        }
                        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    }
                    .disabled(model.connectionRequested || model.discovering)
                }
                if model.localProfiles.isEmpty && !model.discovering {
                    Text("No protected local profiles found. Start Crew from its host console, then refresh discovery.")
                }
                Button("Refresh local discovery") {
                    _Concurrency.Task { await model.discoverLocal() }
                }
                .disabled(model.discovering || model.notice?.endpoint == .authenticated)
                if model.connectionRequested { ProgressView("Connecting") }
            }
            #endif
            Section("Scoped gateway") {
                Text("A scoped Crew gateway connects iPhone, iPad, Mac and Linux to explicitly approved workspaces.")
                Text("Ask the host operator for its public profile. Independently verify the complete TLS fingerprint before connecting.")
                Text("Never paste or scan crew web output into an app or page.")
                    .font(.callout)
                // Replaced by the real native-gateway-bindings setup port.
                // This checkpoint supplies no fabricated profile or enrollment.
                Text("Gateway setup is unavailable in this build.")
                    .foregroundStyle(.secondary)
            }
            if let context = model.context {
                Section("This connection") {
                    Text(context.mode == .localOperator ? "Local operator access" : "Scoped device access")
                    if context.mode == .scopedGateway {
                        Text("Read access may expose content, secrets and paths. Control and Terminal can each direct agents with their existing host permissions. Workspace grants restrict Crew access; they do not create an OS or filesystem sandbox.")
                        if let expiry = context.grant.expiresAtMs { CrewLabeledValue("Access expires", value: CrewLabels.timestamp(expiry)) }
                    }
                    Button("Reconnect") { model.reconnect() }
                    Button("Disconnect") { model.disconnect() }
                    Text("Disconnect closes this app's channels, keeps the connection for later, and does not stop the team or revoke host access.")
                        .font(.callout)
                }
            }
            if let listing = model.workspaces {
                Section("Authorized workspaces") {
                    ForEach(listing.joined, id: \.id) { entry in
                        Button { model.selectWorkspace(entry.id) } label: {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: entry.project ?? entry.workspace).font(.headline)
                                Text(verbatim: entry.workspace).font(.body.monospaced())
                                Text(CrewConnectionLabels.workspace(entry.state))
                                if !entry.detail.isEmpty { Text(verbatim: entry.detail).font(.callout) }
                            }
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        }
                    }
                    if listing.joined.isEmpty {
                        Text("No workspace access. Ask the host operator to approve a workspace for this connection.")
                    }
                }
            }
        }
        .navigationTitle("Connections")
        .accessibilityIdentifier("crew.connections")
    }
}

enum CrewConnectionLabels {
    static func workspace(_ state: WorkspaceState) -> String {
        switch state {
        case .running: return "Running"
        case .stopped: return "Stopped"
        case .incompatible: return "Incompatible"
        case .unreachable: return "Unreachable"
        }
    }
}
