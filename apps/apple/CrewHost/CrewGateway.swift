import CrewBindings
import Foundation

/// Operator input normalization and grouping for the host fingerprint. The
/// connector parses the value and is the only authority on trust; this only
/// removes the separators people copy from a console and groups the echo.
enum CrewFingerprint {
    static let length = 64

    /// The live echo of partial input: ASCII whitespace and colons dropped,
    /// letters lowercased, as the connector's `normalizeFingerprint` does.
    /// Validity is only ever decided by that Rust helper.
    static func echo(_ text: String) -> String {
        String(String.UnicodeScalarView(text.unicodeScalars.filter { scalar in
            !(scalar.isASCII && (scalar == " " || scalar == "\t" || scalar == "\n" || scalar == "\r" || scalar == "\u{0C}" || scalar == ":"))
        })).lowercased()
    }

    /// Four-character groups of a supplied value; nothing is generated.
    static func groups(_ value: String) -> [String] {
        stride(from: 0, to: value.count, by: 4).map { start in
            let lower = value.index(value.startIndex, offsetBy: start)
            let upper = value.index(lower, offsetBy: 4, limitedBy: value.endIndex) ?? value.endIndex
            return String(value[lower..<upper])
        }
    }

    /// Group-by-group presentation of the profile's fingerprint beside the
    /// operator's entry. A difference only withholds the trust action here;
    /// the connector still decides when the values match.
    static func comparison(profile: String, entered: String) -> [CrewFingerprintRow] {
        let left = groups(profile)
        let right = groups(entered)
        return (0..<max(left.count, right.count)).map { index in
            let profileGroup = index < left.count ? left[index] : ""
            let enteredGroup = index < right.count ? right[index] : ""
            return CrewFingerprintRow(index: index + 1, profile: profileGroup, entered: enteredGroup, differs: profileGroup != enteredGroup)
        }
    }
}

struct CrewFingerprintRow: Equatable {
    let index: Int
    let profile: String
    let entered: String
    let differs: Bool
}

/// The comparison code arrives as 16 hex digits after the connector has
/// verified it against its transcript; it is shown as four literal groups.
enum CrewComparisonCode {
    static func groups(_ code: String) -> [String] { CrewFingerprint.groups(code) }
}

/// Safe, typed connector reasons. Where the experience contract has fixed
/// copy for a code it is used; otherwise the connector's own message.
enum CrewGatewayCopy {
    static let trustFailure = "Host identity could not be verified. Compare the profile with crew native status at the trusted host console."

    static func failure(_ error: Error) -> String {
        guard case GatewayError.Failure(let code, let message)? = error as? GatewayError else {
            return "Device setup could not complete. Try again explicitly."
        }
        return reason(code: code, message: message)
    }

    static func reason(code: String, message: String) -> String {
        switch code {
        case "invalid_profile": return "Check the host profile. \(message)"
        case "tls_trust": return trustFailure
        case "key_locked": return "Unlock this device's credential storage and try again."
        case "setup_busy": return "Device setup is already in progress in this window."
        default: return message
        }
    }

    static func progress(_ progress: EnrollmentProgress) -> String {
        switch progress {
        case .connecting: return "Requesting access from the checked host"
        case .awaitingApproval: return "Waiting for the host operator to compare and approve"
        }
    }

    static func outcome(_ outcome: EnrollmentOutcome) -> String {
        switch outcome {
        case .approved: return "Access granted"
        case .canceled: return "Request closed."
        case .outcomeUnknown:
            return "The connection closed before approval was confirmed here. The host may have approved this device. Reconnect to check, or revoke it at the host."
        case .failed(let reason): return reason.message
        }
    }
}

/// Public, presentation-safe gateway profile fields kept in ordinary
/// preferences. There is no private key, store handle, token, proof or
/// signature here. The profile ID also names the device key in the OS store.
struct CrewGatewayRecord: Codable, Equatable, Identifiable {
    let id: String
    var connectionName: String
    let endpointId: String
    let name: String
    let url: String
    let tlsLeafSha256: String
    let trustPem: String
    /// The operator's independently entered fingerprint, kept so a later
    /// launch can re-establish trust without re-reading the imported field.
    let verifiedFingerprint: String
    var deviceLabel: String?

    init(id: String, connectionName: String, profile: PublicProfile, verifiedFingerprint: String) {
        self.id = id
        self.connectionName = connectionName
        endpointId = profile.endpointId
        name = profile.name
        url = profile.url
        tlsLeafSha256 = profile.tlsLeafSha256
        trustPem = profile.trustPem
        self.verifiedFingerprint = verifiedFingerprint
    }

    var publicProfile: PublicProfile {
        PublicProfile(endpointId: endpointId, name: name, url: url, tlsLeafSha256: tlsLeafSha256, trustPem: trustPem)
    }

    var keyReference: UUID? { UUID(uuidString: id) }
}

/// Saved public gateway profiles. Writes replace the whole bounded list.
struct CrewGatewayStore {
    static let key = "crew.gatewayProfiles"
    static let limit = 32
    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    func load() -> [CrewGatewayRecord] {
        guard let data = defaults.data(forKey: Self.key),
              let records = try? JSONDecoder().decode([CrewGatewayRecord].self, from: data) else { return [] }
        return Array(records.prefix(Self.limit))
    }

    func save(_ records: [CrewGatewayRecord]) {
        guard let data = try? JSONEncoder().encode(Array(records.prefix(Self.limit))) else { return }
        defaults.set(data, forKey: Self.key)
    }

    func upsert(_ record: CrewGatewayRecord) {
        var records = load().filter { $0.id != record.id }
        records.append(record)
        save(records)
    }

    func remove(_ id: String) { save(load().filter { $0.id != id }) }
}

/// The host's device-key operations, confined to CrewHost. Transcripts are
/// the Rust-constructed typed objects; there is no arbitrary byte signer.
/// Failures are thrown as `CrewSignerFailure`; anything else is reported to
/// the connector as unavailable storage.
protocol CrewDeviceKeys: Sendable {
    func devicePublicKey(reference: UUID) async throws -> Data
    func signAuthentication(_ transcript: any AuthTranscriptProtocol, reference: UUID) async throws -> Data
    func signEnrollment(_ transcript: any EnrollTranscriptProtocol, reference: UUID) async throws -> Data
    func retireDeviceKey(reference: UUID) async throws
}

/// The connector's typed signer error, carried as a Swift error.
struct CrewSignerFailure: Error, Equatable {
    let error: SignerError
}

/// UniFFI callback adapter. Each request is enqueued and returns at once;
/// its one-shot reply is completed from the key service. Replies the Rust
/// side has already canceled are left alone, and late completion errors are
/// ignored because the connector owns the attempt's outcome.
final class CrewDeviceSigner: DeviceSigner, @unchecked Sendable {
    private let keys: any CrewDeviceKeys
    private let reference: UUID

    init(keys: any CrewDeviceKeys, reference: UUID) {
        self.keys = keys
        self.reference = reference
    }

    func publicKey(reply: PublicKeyReply) {
        let keys = keys, reference = reference
        _Concurrency.Task {
            guard reply.isPending() else { return }
            do {
                let bytes = try await keys.devicePublicKey(reference: reference)
                try? reply.complete(bytes: bytes)
            } catch {
                try? reply.fail(error: Self.signerError(error))
            }
        }
    }

    func signAuth(transcript: AuthTranscript, reply: SignatureReply) {
        let keys = keys, reference = reference
        _Concurrency.Task {
            guard reply.isPending() else { return }
            do {
                let bytes = try await keys.signAuthentication(transcript, reference: reference)
                try? reply.complete(bytes: bytes)
            } catch {
                try? reply.fail(error: Self.signerError(error))
            }
        }
    }

    func signEnrollment(transcript: EnrollTranscript, reply: SignatureReply) {
        let keys = keys, reference = reference
        _Concurrency.Task {
            guard reply.isPending() else { return }
            do {
                let bytes = try await keys.signEnrollment(transcript, reference: reference)
                try? reply.complete(bytes: bytes)
            } catch {
                try? reply.fail(error: Self.signerError(error))
            }
        }
    }

    func retire(reply: RetirementReply) {
        let keys = keys, reference = reference
        _Concurrency.Task {
            guard reply.isPending() else { return }
            do {
                try await keys.retireDeviceKey(reference: reference)
                try? reply.complete()
            } catch {
                try? reply.fail(error: Self.signerError(error))
            }
        }
    }

    private static func signerError(_ error: Error) -> SignerError {
        (error as? CrewSignerFailure)?.error ?? .unavailable
    }
}

/// Setup callbacks arrive on a Rust observer thread, in order, with exactly
/// one terminal callback per returned handle. They are handed to the main
/// executor synchronously, like client notices, to keep that order.
final class CrewSetupObserver: SetupObserver, @unchecked Sendable {
    private let handler: @MainActor @Sendable (CrewSetupEvent) -> Void

    init(_ handler: @escaping @MainActor @Sendable (CrewSetupEvent) -> Void) { self.handler = handler }

    func changed(update: EnrollmentUpdate) { handoff(.changed(update)) }
    func finished(result: EnrollmentResult) { handoff(.finished(result)) }
    func failed(reason: ClientReason) { handoff(.failed(reason)) }

    private func handoff(_ event: CrewSetupEvent) {
        let handler = handler
        if Thread.isMainThread {
            MainActor.assumeIsolated { handler(event) }
        } else {
            DispatchQueue.main.sync { MainActor.assumeIsolated { handler(event) } }
        }
    }
}

enum CrewSetupEvent: Equatable {
    case changed(EnrollmentUpdate)
    case finished(EnrollmentResult)
    case failed(ClientReason)
}
