import SwiftUI

/// S02 Host profile. Public input only: the host submits it to the real Rust
/// profile parser and fingerprint check. Entering a profile never connects
/// or grants access, and the fingerprint is never filled from the profile.
public struct CrewPublicProfileForm: View {
    public enum Field: Hashable { case name, profile, fingerprint }

    @Binding private var name: String
    @Binding private var profileText: String
    @Binding private var fingerprint: String
    private let fingerprintEcho: [String]
    private let fingerprintCount: Int
    private let pending: Bool
    private let profileIssue: String?
    private let fingerprintIssue: String?
    private let focusRequest: (field: Field, id: UUID)?
    private let review: () -> Void
    private let cancel: () -> Void
    @FocusState private var field: Field?
    @ScaledMetric(relativeTo: .body) private var profileHeight: CGFloat = 160

    /// `fingerprintEcho` is the caller's grouped echo of the entered value
    /// with separators removed; `fingerprintCount` is its character count.
    public init(name: Binding<String>, profileText: Binding<String>, fingerprint: Binding<String>, fingerprintEcho: [String], fingerprintCount: Int, pending: Bool, profileIssue: String?, fingerprintIssue: String?, focusRequest: (field: Field, id: UUID)?, review: @escaping () -> Void, cancel: @escaping () -> Void) {
        self._name = name; self._profileText = profileText; self._fingerprint = fingerprint
        self.fingerprintEcho = fingerprintEcho; self.fingerprintCount = fingerprintCount
        self.pending = pending; self.profileIssue = profileIssue; self.fingerprintIssue = fingerprintIssue
        self.focusRequest = focusRequest; self.review = review; self.cancel = cancel
    }

    public var body: some View {
        Form {
            Section {
                TextField("Connection name", text: $name)
                    .focused($field, equals: .name)
                Text("A local description for this list. It does not verify the host.").font(.callout)
            }
            Section("Public host profile") {
                Text("Paste the output of crew native status from the host.").font(.callout)
                TextEditor(text: $profileText)
                    .font(.body.monospaced())
                    .frame(minHeight: profileHeight)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .focused($field, equals: .profile)
                    .accessibilityLabel("Public host profile")
                if let profileIssue {
                    Text(verbatim: profileIssue).foregroundStyle(CrewPalette.danger).textSelection(.enabled)
                        .accessibilityIdentifier("crew.profile-error")
                }
            }
            Section("Host fingerprint") {
                TextField("Host fingerprint", text: $fingerprint, axis: .vertical)
                    .font(.body.monospaced())
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    #endif
                    .focused($field, equals: .fingerprint)
                Text("From crew native status at the trusted host console. Do not copy it from the profile above.").font(.callout)
                if !fingerprintEcho.isEmpty {
                    Text(verbatim: fingerprintEcho.joined(separator: " "))
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityLabel(crewSpelledLabel("Entered fingerprint, \(fingerprintEcho.count) groups", fingerprintEcho.joined(separator: " ")))
                }
                Text("\(fingerprintCount) of 64").font(.callout).foregroundStyle(.secondary)
                if let fingerprintIssue {
                    Text(verbatim: fingerprintIssue).foregroundStyle(CrewPalette.danger).textSelection(.enabled)
                        .accessibilityIdentifier("crew.fingerprint-error")
                }
            }
            Section {
                Text("Never paste or scan crew web output into this app. This form accepts public host information, not a browser code, token or private key.")
                    .font(.callout)
                if pending { ProgressView("Checking public host profile") }
                Button { review() } label: { Text("Verify this host").frame(minHeight: 44) }.disabled(pending)
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
        }
        .navigationTitle("Connect to Crew")
        .onChange(of: focusRequest?.id) { _, _ in if let target = focusRequest?.field { field = target } }
    }
}

/// One aligned group of the two fingerprints on Verify this host.
public struct CrewFingerprintGroupRow: Identifiable, Equatable {
    public let index: Int
    public let profile: String
    public let entered: String
    public let differs: Bool
    public var id: Int { index }

    public init(index: Int, profile: String, entered: String, differs: Bool) {
        self.index = index; self.profile = profile; self.entered = entered; self.differs = differs
    }
}

/// S02b Verify this host. The profile's fingerprint and the operator's entry
/// are shown group by group, with each difference stated in text. A local
/// difference withholds the trust action; with matching values the connector
/// still performs the authoritative comparison and every TLS check.
public struct CrewHostTrustReview: View {
    private let name: String
    private let endpoint: String
    private let endpointID: String
    private let rows: [CrewFingerprintGroupRow]
    private let pending: Bool
    private let issue: String?
    private let trust: () -> Void
    private let reviewProfile: () -> Void
    private let cancel: () -> Void

    public init(name: String, endpoint: String, endpointID: String, rows: [CrewFingerprintGroupRow], pending: Bool, issue: String?, trust: @escaping () -> Void, reviewProfile: @escaping () -> Void, cancel: @escaping () -> Void) {
        self.name = name; self.endpoint = endpoint; self.endpointID = endpointID; self.rows = rows
        self.pending = pending; self.issue = issue
        self.trust = trust; self.reviewProfile = reviewProfile; self.cancel = cancel
    }

    private var matches: Bool { !rows.isEmpty && !rows.contains(where: \.differs) }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Verify this host").font(.title2.bold()).accessibilityAddTraits(.isHeader)
                CrewLabeledValue("Profile name", value: name)
                CrewLabeledValue("Endpoint", value: endpoint, monospaced: true)
                CrewLabeledValue("Endpoint UUID", value: endpointID, monospaced: true, spellsOut: true)
                // Monospaced four-character groups align by column without a
                // Grid, so each row can be one accessibility element.
                VStack(alignment: .leading, spacing: 4) {
                    Text("Fingerprints").font(.headline).accessibilityAddTraits(.isHeader)
                    Text("Each row: group number, the profile fingerprint, then the value you entered.").font(.callout)
                    ForEach(rows) { row in
                        HStack(spacing: 16) {
                            Text(verbatim: String(format: "%2d", row.index)).font(.body.monospaced()).foregroundStyle(.secondary)
                            Text(verbatim: row.profile).speechSpellsOutCharacters().font(.body.monospaced())
                            Text(verbatim: row.entered.isEmpty ? "----" : row.entered).speechSpellsOutCharacters().font(.body.monospaced())
                            if row.differs { Text("differs").font(.callout.bold()).foregroundStyle(CrewPalette.danger) }
                        }
                        .textSelection(.enabled)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(row.differs
                            ? crewSpelledLabel("Group \(row.index) differs. Profile", row.profile) + Text(verbatim: ". ") + (row.entered.isEmpty ? Text("Nothing entered") : crewSpelledLabel("Entered", row.entered))
                            : crewSpelledLabel("Group \(row.index)", row.profile))
                    }
                }
                Text("If the host loses its private runtime state, such as after logout or reboot, this profile cannot reconnect automatically. The host operator must provision the endpoint again and approve a new device key.")
                    .font(.callout)
                if let issue {
                    Text(verbatim: issue).foregroundStyle(CrewPalette.danger).textSelection(.enabled)
                }
                if matches && issue == nil {
                    Text("TLS name, certificate chain, signature, validity and pin verification still run before any device proof.")
                        .font(.callout)
                    if pending { ProgressView("Checking host trust") }
                    Button(action: trust) { Text("Trust this host").frame(minHeight: 44) }
                        .buttonStyle(.borderedProminent)
                        .disabled(pending)
                } else {
                    if !matches {
                        Text("Host identity could not be verified. Compare the profile with crew native status at the trusted host console.")
                    }
                    Button(action: reviewProfile) { Text("Review host profile").frame(minHeight: 44) }
                }
                Button(action: cancel) { Text("Cancel").frame(minHeight: 44) }
            }
            .padding(24).frame(maxWidth: 680, alignment: .leading)
        }
        .navigationTitle("Connect to Crew")
    }
}
