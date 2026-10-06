import CrewBindings
import CrewUI
import SwiftUI

/// Local profiles come from CrewClient.local's protected discovery snapshot.
/// Selecting an opaque profile calls the real facade; no registry, socket path,
/// daemon identity or workspace is manufactured in Swift. Gateway profiles are
/// the saved public profiles; connecting re-establishes trust in Rust.
@MainActor
struct CrewConnectionsView: View {
    @Bindable var model: CrewSessionModel
    @State private var gateways = CrewGatewayStore().load()
    @State private var setup: CrewGatewaySetup?
    @State private var gatewayIssue: String?
    @State private var gatewayBusy = false
    @State private var pendingRemoval: CrewGatewayRecord?

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
                Text("Never paste or scan crew web output into an app or page.")
                    .font(.callout)
                ForEach(gateways) { record in gatewayRow(record) }
                if let gatewayIssue { Text(verbatim: gatewayIssue).textSelection(.enabled) }
                Button("Add remote connection", systemImage: "plus") { setup = CrewGatewaySetup(model: model) }
                    .disabled(gatewayBusy)
            }
            if let context = model.context {
                Section(context.mode == .localOperator ? "This connection" : accessTitle(context)) {
                    Text(context.mode == .localOperator ? "Local operator · full Crew access" : "Scoped device access")
                    if context.mode == .scopedGateway {
                        CrewLabeledValue("Read", value: context.grant.tiers.contains(.read) ? "Granted" : "Not granted")
                        CrewLabeledValue("Control", value: context.grant.tiers.contains(.control) ? "Granted" : "Not granted")
                        CrewLabeledValue("Terminal", value: context.grant.tiers.contains(.terminal) ? "Granted" : "Not granted")
                        if let expiry = context.grant.expiresAtMs { CrewLabeledValue("Access expires", value: CrewLabels.timestampWithZone(expiry)) }
                        CrewAccessDisclosure()
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
        // Removal deletes this device's key for the host; Cancel is the default.
        .confirmationDialog(
            pendingRemoval.map { "Remove \($0.connectionName) from this app?" } ?? "Remove from this app?",
            isPresented: Binding(get: { pendingRemoval != nil }, set: { if !$0 { pendingRemoval = nil } }),
            titleVisibility: .visible,
            presenting: pendingRemoval
        ) { record in
            Button("Remove from this app", role: .destructive) {
                pendingRemoval = nil
                remove(record)
            }
            Button("Cancel", role: .cancel) { pendingRemoval = nil }
                .keyboardShortcut(.defaultAction)
        } message: { _ in
            Text("This deletes this device's key for this host. Host access remains until revoked or it expires.")
        }
        .navigationDestination(isPresented: Binding(
            get: { setup != nil },
            set: { presented in
                guard !presented else { return }
                setup?.close()
                setup = nil
                gateways = CrewGatewayStore().load()
            }
        )) {
            if let setup {
                CrewGatewaySetupView(setup: setup, model: model) {
                    setup.close()
                    self.setup = nil
                    gateways = CrewGatewayStore().load()
                }
            }
        }
    }

    /// S01 row and S08 details for one saved gateway profile.
    @ViewBuilder private func gatewayRow(_ record: CrewGatewayRecord) -> some View {
        let current = model.gatewayProfileId == record.id
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: record.connectionName).font(.headline)
            Text("Scoped gateway").font(.callout)
            Text(verbatim: record.url).font(.body.monospaced()).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            if current { Text(verbatim: CrewFailure.workspace(model.notice?.workspace ?? .noneSelected)).font(.callout) }
            Button(current ? "Reconnect" : "Connect") { connect(record) }
                .disabled(gatewayBusy || model.connectionRequested)
            DisclosureGroup("Connection details") {
                CrewLabeledValue("Profile name", value: record.name)
                CrewLabeledValue("Endpoint UUID", value: record.endpointId, monospaced: true, spellsOut: true)
                if let label = record.deviceLabel { CrewLabeledValue("Device label", value: label) }
                Text("How to revoke access").font(.headline)
                Text("At the trusted host console, use crew native devices to find this device, then crew native revoke. This app cannot revoke its own access remotely.")
                Text("Remove from this app removes this profile and retires this app's device key. Host access remains until revoked or expired.")
                    .font(.callout)
                Button("Request access with this profile") {
                    let flow = CrewGatewaySetup(model: model)
                    flow.resume(record)
                    setup = flow
                }
                .disabled(gatewayBusy)
                Button("Remove from this app", role: .destructive) { pendingRemoval = record }
                    .disabled(gatewayBusy)
            }
        }
        .padding(.vertical, 4)
    }

    /// The authenticated grant is for the current workspace connection; the
    /// listing carries no per-workspace powers, so the title names it.
    private func accessTitle(_ context: ConnectionContext) -> String {
        let entry = model.workspaces?.joined.first { $0.id == context.scope.workspaceId }
        guard let name = entry?.project ?? entry?.workspace else { return "Access in the current workspace" }
        return "Access in \(name)"
    }

    private func connect(_ record: CrewGatewayRecord) {
        gatewayBusy = true
        gatewayIssue = nil
        _Concurrency.Task {
            do { try await CrewGatewayProfiles.connect(record, model: model) }
            catch { gatewayIssue = CrewGatewayCopy.failure(error) }
            gatewayBusy = false
        }
    }

    private func remove(_ record: CrewGatewayRecord) {
        gatewayBusy = true
        gatewayIssue = nil
        _Concurrency.Task {
            do { try await CrewGatewayProfiles.remove(record, model: model) }
            catch { gatewayIssue = error is AppleDeviceKeyFailure ? CrewGatewaySetup.storage(error) : CrewGatewayCopy.failure(error) }
            gateways = CrewGatewayStore().load()
            gatewayBusy = false
        }
    }
}

/// The Connect to Crew pages, driven by one setup coordinator.
@MainActor
struct CrewGatewaySetupView: View {
    @Bindable var setup: CrewGatewaySetup
    let model: CrewSessionModel
    let done: () -> Void

    var body: some View {
        Group {
            switch setup.step {
            case .profile:
                let echo = CrewFingerprint.echo(setup.fingerprintText)
                CrewPublicProfileForm(name: $setup.connectionName, profileText: $setup.profileText, fingerprint: $setup.fingerprintText, fingerprintEcho: CrewFingerprint.groups(echo), fingerprintCount: echo.count, pending: setup.pending, profileIssue: setup.profileIssue, fingerprintIssue: setup.fingerprintIssue, focusRequest: profileFocus, review: setup.review, cancel: done)
            case .verify(let profile, let entered):
                CrewHostTrustReview(name: profile.name, endpoint: profile.url, endpointID: profile.endpointId, rows: CrewFingerprint.comparison(profile: profile.tlsLeafSha256, entered: entered).map { CrewFingerprintGroupRow(index: $0.index, profile: $0.profile, entered: $0.entered, differs: $0.differs) }, pending: setup.pending, issue: setup.issue, trust: setup.trustHost, reviewProfile: setup.reviewProfile, cancel: done)
            case .device:
                CrewDeviceNameForm(name: $setup.deviceLabel, storageDescription: setup.existingKey
                    ? "This device already has a key for this host from an earlier request. It is reused; no new key is created."
                    : "A new device key is created in this device's Keychain, usable only on this device while it is unlocked.",
                    storageReady: setup.storageReady, pending: setup.pending, nameIssue: setup.deviceLabelIssue, issue: setup.issue, focusRequest: setup.focusRequest?.field == .deviceLabel ? setup.focusRequest?.id : nil, proceed: setup.continueWithDevice, cancel: done)
            case .request:
                CrewRequestAccessView(host: host, device: setup.deviceLabel, pending: setup.pending, stage: setup.progress, issue: setup.issue, request: setup.requestAccess, cancel: setup.enrolling ? setup.cancelRequest : done)
            case .compare(let groups, let expires):
                CrewCompareAtHostView(host: host, device: setup.deviceLabel, groups: groups, status: "\(setup.progress ?? "Waiting for the host operator") · Request expires \(CrewLabels.timestamp(expires))", cancel: setup.cancelRequest)
            case .granted(let access):
                granted(access)
            case .closed(let message, let mayHaveApproved):
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        Text(verbatim: message).textSelection(.enabled)
                        if mayHaveApproved {
                            Button { setup.reconnectToCheck(); done() } label: { Text("Reconnect to check").frame(minHeight: 44) }
                                .buttonStyle(.borderedProminent)
                        }
                        Button(action: done) { Text("Done").frame(minHeight: 44) }
                    }
                    .padding(24).frame(maxWidth: 680, alignment: .leading)
                }
                .navigationTitle("Connect to Crew")
            }
        }
        .onDisappear { setup.close() }
    }

    private var host: String {
        guard let record = setup.record else { return "" }
        return "\(record.connectionName) · \(record.url)"
    }

    private var profileFocus: (field: CrewPublicProfileForm.Field, id: UUID)? {
        guard let request = setup.focusRequest else { return nil }
        switch request.field {
        case .name: return (.name, request.id)
        case .profile: return (.profile, request.id)
        case .fingerprint: return (.fingerprint, request.id)
        case .deviceLabel: return nil
        }
    }

    /// S07 Access granted: only returned memberships, never invented while
    /// the listing is loading, and no names of other workspaces.
    private func granted(_ access: GatewayDeviceAccess) -> some View {
        List {
            Section("Access granted") {
                CrewLabeledValue("Host", value: host)
                CrewLabeledValue("Device", value: setup.deviceLabel)
                CrewLabeledValue("Device ID", value: access.deviceId, monospaced: true, spellsOut: true)
                CrewLabeledValue("Access expires", value: CrewLabels.timestampWithZone(access.credentialExpiresAtMs))
            }
            Section("Workspaces") {
                if let listing = model.workspaces {
                    ForEach(listing.joined, id: \.id) { entry in
                        Button {
                            model.selectWorkspace(entry.id)
                            done()
                        } label: {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: entry.project ?? entry.workspace).font(.headline)
                                Text(CrewConnectionLabels.workspace(entry.state))
                                Text("Open workspace").font(.callout)
                            }
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        }
                    }
                    if listing.joined.isEmpty {
                        Text("No workspace access. Ask the host operator to approve a workspace for this device.")
                    }
                } else {
                    ProgressView("Loading workspaces")
                }
            }
            Section { CrewAccessDisclosure() }
            Section { Button("Done", action: done) }
        }
        .navigationTitle("Connect to Crew")
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
