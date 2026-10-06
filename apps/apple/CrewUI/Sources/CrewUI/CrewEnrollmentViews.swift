import Accessibility
import SwiftUI

/// UI input and instructions only. The native connector owns label validation,
/// key readiness, enrollment attempts and their bounded lifetime.
public struct CrewDeviceNameForm: View {
    @Binding private var name: String
    private let storageDescription: String
    private let storageReady: Bool
    private let pending: Bool
    private let issue: String?
    private let proceed: () -> Void
    private let cancel: () -> Void

    public init(name: Binding<String>, storageDescription: String, storageReady: Bool, pending: Bool, issue: String?, proceed: @escaping () -> Void, cancel: @escaping () -> Void) {
        self._name = name; self.storageDescription = storageDescription; self.storageReady = storageReady
        self.pending = pending; self.issue = issue; self.proceed = proceed; self.cancel = cancel
    }

    public var body: some View {
        Form {
            Section("Name this device") {
                TextField("Device name", text: $name, prompt: Text("Work phone"))
                Text("This label helps the host operator recognize your request; it does not verify the device.")
            }
            Section("Secure storage") {
                Text(verbatim: storageDescription)
                if pending { ProgressView("Checking this device's secure storage") }
                if let issue { Text(verbatim: issue).textSelection(.enabled) }
                Text("The device signing key stays in this device's protected credential store. It is never imported from the host or displayed here.")
            }
            Section {
                Button(action: proceed) { Text("Continue").frame(minHeight: 44) }.disabled(!storageReady || pending)
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
        }
        .navigationTitle("Connect to Crew")
    }
}

public struct CrewRequestAccessView: View {
    private let host: String
    private let device: String
    private let pending: Bool
    private let stage: String?
    private let issue: String?
    private let request: () -> Void
    private let cancel: () -> Void

    public init(host: String, device: String, pending: Bool, stage: String?, issue: String?, request: @escaping () -> Void, cancel: @escaping () -> Void) {
        self.host = host; self.device = device; self.pending = pending; self.stage = stage; self.issue = issue
        self.request = request; self.cancel = cancel
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Request this device's access").font(.title2.bold())
                CrewLabeledValue("Checked host", value: host)
                CrewLabeledValue("Device label", value: device)
                Text("At the trusted host console, open an enrollment window with crew native enroll, then request access here.")
                Text("The host operator chooses the workspaces and access powers. This app cannot open or extend the host window, approve a device, or grant itself access.")
                if pending { ProgressView(stage ?? "Requesting access") }
                if let issue { Text(verbatim: issue).textSelection(.enabled) }
                Button(action: request) { Text("Request access").frame(minHeight: 44) }.disabled(pending)
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
            .padding(24).frame(maxWidth: 680, alignment: .leading)
        }
        .navigationTitle("Connect to Crew")
    }
}

/// Requires the caller's actual validated accepted-intent code. No code is
/// predicted/generated in Swift; all groups and leading zeroes remain literal.
public struct CrewCompareAtHostView: View {
    private let host: String
    private let device: String
    private let groups: [String]
    private let status: String
    private let cancel: () -> Void

    public init(host: String, device: String, groups: [String], status: String, cancel: @escaping () -> Void) {
        self.host = host; self.device = device; self.groups = groups; self.status = status; self.cancel = cancel
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Compare at the trusted host").font(.title2.bold())
                CrewLabeledValue("Checked host", value: host)
                CrewLabeledValue("Untrusted device label", value: device)
                Text("Comparison code").font(.headline)
                // Adaptive grid wraps only between whole supplied groups.
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 92), alignment: .leading)], alignment: .leading, spacing: 12) {
                    ForEach(Array(groups.enumerated()), id: \.offset) { index, group in
                        Text(verbatim: group).font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityLabel("Comparison group \(index + 1): \(group)")
                    }
                }
                Text("Compare all four groups with this request at the trusted host console. Approve access there only if they match.")
                Text("A device name does not verify the request. Only the host operator can approve the selected workspaces and powers.")
                Text(verbatim: status).font(.callout)
                Button(action: cancel) { Text("Cancel request").frame(minHeight: 44) }
            }
            .padding(24).frame(maxWidth: 680, alignment: .leading)
        }
        .navigationTitle("Connect to Crew")
        // Announced once when the verified code appears, not per status tick.
        .onAppear { AccessibilityNotification.Announcement("Comparison code ready. Compare all four groups at the trusted host console.").post() }
    }
}

public struct CrewAccessDisclosure: View {
    public init() {}
    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("This device's access").font(.headline)
            Text("Read may expose workspace content, secrets and paths.")
            Text("Control includes messages and answers that can direct agents to act with their existing host permissions, as well as agent and team controls.")
            Text("Terminal permits terminal interaction with an agent's existing host permissions.")
            Text("Workspace grants restrict Crew access. They do not create an OS sandbox or confine an agent's filesystem access to that workspace.")
            Text("Read is the default. Control and Terminal are separate powers that the host operator must explicitly grant.")
        }
        .textSelection(.enabled)
        .fixedSize(horizontal: false, vertical: true)
    }
}
