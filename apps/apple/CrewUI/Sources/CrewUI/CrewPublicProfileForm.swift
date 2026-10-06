import SwiftUI

/// Public input fields only. The host submits these values to the real Rust
/// profile/trust port; parsing, canonicalization and TLS checks are not done in
/// this presentation. Entering a profile never connects or grants access.
public struct CrewPublicProfileForm: View {
    @Binding private var name: String
    @Binding private var endpointID: String
    @Binding private var endpointURL: String
    @Binding private var trustPEM: String
    @Binding private var fingerprint: String
    private let pending: Bool
    private let error: String?
    private let review: () -> Void
    private let cancel: () -> Void
    @FocusState private var field: Field?

    public init(name: Binding<String>, endpointID: Binding<String>, endpointURL: Binding<String>, trustPEM: Binding<String>, fingerprint: Binding<String>, pending: Bool, error: String?, review: @escaping () -> Void, cancel: @escaping () -> Void) {
        self._name = name; self._endpointID = endpointID; self._endpointURL = endpointURL
        self._trustPEM = trustPEM; self._fingerprint = fingerprint
        self.pending = pending; self.error = error; self.review = review; self.cancel = cancel
    }

    public var body: some View {
        Form {
            Section("Public host profile") {
                TextField("Connection name", text: $name).focused($field, equals: .name)
                Text("This is a local description, not a verified host identity.").font(.callout)
                TextField("Endpoint UUID", text: $endpointID).font(.body.monospaced()).autocorrectionDisabled().focused($field, equals: .endpointID)
                TextField("wss:// endpoint ending in /native/ws", text: $endpointURL, axis: .vertical).font(.body.monospaced()).autocorrectionDisabled().focused($field, equals: .endpointURL)
                TextField("Expected SHA-256 fingerprint", text: $fingerprint, axis: .vertical).font(.body.monospaced()).autocorrectionDisabled().focused($field, equals: .fingerprint)
                Text("Public certificate / trust material (PEM)").font(.headline)
                TextEditor(text: $trustPEM).font(.body.monospaced()).frame(minHeight: 160).focused($field, equals: .trustPEM)
                    .accessibilityLabel("Public certificate or trust material in PEM format")
            }
            Section("Before connecting") {
                Text("Compare the complete fingerprint with crew native status at the trusted host console. A parsed or imported profile has not yet been trusted.")
                Text("Never paste or scan crew web output into this app. This form accepts public host information, not a browser code, token or private key.")
                Text("If the host loses its private runtime state, such as after logout or reboot, this profile cannot reconnect automatically. The host operator must provision the endpoint again and approve a new device key.")
            }
            if let error {
                Section("Profile issue") {
                    Text(verbatim: error).textSelection(.enabled)
                        .accessibilityIdentifier("crew.profile-error")
                }
            }
            Section {
                if pending { ProgressView("Checking public host profile") }
                Button { review() } label: { Text("Review this host").frame(minHeight: 44) }.disabled(pending)
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
        }
        .navigationTitle("Host profile")
    }

    private enum Field: Hashable { case name, endpointID, endpointURL, trustPEM, fingerprint }
}

/// Full fingerprint supplied by the validated public-profile port. A user
/// acknowledgement does not replace the connector's name/chain/validity/pin
/// verification and never offers a trust-failure bypass.
public struct CrewHostTrustReview: View {
    private let name: String
    private let endpoint: String
    private let fingerprint: String
    private let accept: () -> Void
    private let cancel: () -> Void
    @State private var checked = false

    public init(name: String, endpoint: String, fingerprint: String, accept: @escaping () -> Void, cancel: @escaping () -> Void) {
        self.name = name; self.endpoint = endpoint; self.fingerprint = fingerprint
        self.accept = accept; self.cancel = cancel
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Verify this host").font(.title2.bold())
                CrewLabeledValue("Connection name", value: name)
                CrewLabeledValue("Endpoint", value: endpoint, monospaced: true)
                Text("Complete expected SHA-256 fingerprint").font(.headline)
                Text(verbatim: fingerprint).font(.body.monospaced()).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel("Complete expected SHA-256 fingerprint")
                Text("Compare this fingerprint with crew native status at the trusted host console. Do not rely on the network peer or this connection name to verify it.")
                Toggle("I checked the fingerprint independently", isOn: $checked)
                Text("TLS name, certificate chain, signature, validity and pin verification still run before any device proof. A changed or invalid certificate cannot be accepted here.")
                Button(action: accept) { Text("Use this checked public profile").frame(minHeight: 44) }.disabled(!checked)
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
            .padding(24).frame(maxWidth: 680, alignment: .leading)
        }
        .navigationTitle("Host trust")
    }
}
