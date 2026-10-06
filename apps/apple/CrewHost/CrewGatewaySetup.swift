import CrewBindings
import Foundation
import Observation

/// One window's Connect to Crew flow (S02–S07) over the real gateway ports.
/// Profile parsing, fingerprint validity, trust, TLS, proofs, enrollment and
/// grants all stay in Rust; this holds the operator's non-secret input, the
/// current step and the one live enrollment handle. Nothing here predicts a
/// comparison code, approves a device or retries an attempt.
@MainActor
@Observable
final class CrewGatewaySetup {
    enum Step: Equatable {
        case profile
        case verify(PublicProfile, entered: String)
        case device
        case request
        case compare(groups: [String], expiresAtMs: Int64)
        case granted(GatewayDeviceAccess)
        case closed(String, mayHaveApproved: Bool)
    }

    enum Field: Hashable { case name, profile, fingerprint, deviceLabel }

    private let model: CrewSessionModel
    private let vault: AppleDeviceKeyVault
    private let store: CrewGatewayStore

    var connectionName = ""
    var profileText = ""
    var fingerprintText = ""
    var deviceLabel = ""
    private(set) var step: Step = .profile
    private(set) var profileIssue: String?
    private(set) var fingerprintIssue: String?
    private(set) var issue: String?
    private(set) var pending = false
    private(set) var progress: String?
    private(set) var storageReady = false
    private(set) var existingKey = false
    /// A field that should receive focus after an explicit failed submit.
    /// Background updates never set it.
    private(set) var focusRequest: (field: Field, id: UUID)?
    private(set) var record: CrewGatewayRecord?
    private var trusted: GatewayProfile?
    private var handle: EnrollmentHandle?
    private var attempt = UUID()

    init(model: CrewSessionModel, vault: AppleDeviceKeyVault = AppleDeviceKeyVault(), store: CrewGatewayStore = CrewGatewayStore()) {
        self.model = model
        self.vault = vault
        self.store = store
    }

    var enrolling: Bool { handle != nil }

    // MARK: S02 Host profile

    func review() {
        profileIssue = nil
        fingerprintIssue = nil
        issue = nil
        let text = profileText
        let entered = fingerprintText
        pending = true
        let attempt = attempt
        _Concurrency.Task {
            // Synchronous FFI never runs on the main actor.
            let parsed = await _Concurrency.Task.detached { () -> (Result<PublicProfile, Error>, String?) in
                (Result { try parseGatewayProfile(text: text) }, normalizeFingerprint(text: entered))
            }.value
            guard self.attempt == attempt else { return }
            pending = false
            switch parsed.0 {
            case .failure(let error):
                profileIssue = CrewGatewayCopy.failure(error)
                request(focus: .profile)
                return
            case .success(let profile):
                guard let normalized = parsed.1 else {
                    fingerprintIssue = "The fingerprint has 64 hexadecimal characters (0–9, a–f). This one has \(CrewFingerprint.echo(entered).count)."
                    request(focus: .fingerprint)
                    return
                }
                if connectionName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { connectionName = profile.name }
                step = .verify(profile, entered: normalized)
            }
        }
    }

    /// Back to the profile form, focusing the fingerprint the operator entered.
    func reviewProfile() {
        issue = nil
        step = .profile
        request(focus: .fingerprint)
    }

    // MARK: S02b Verify this host

    func trustHost() {
        guard case .verify(let profile, let entered) = step, !pending else { return }
        issue = nil
        pending = true
        let id = UUID().uuidString
        let attempt = attempt
        _Concurrency.Task {
            let result = await _Concurrency.Task.detached {
                Result { try GatewayProfile.trust(id: id, profile: profile, verifiedFingerprint: entered) }
            }.value
            guard self.attempt == attempt else { return }
            pending = false
            switch result {
            case .failure(let error):
                if case GatewayError.Failure(let code, _)? = error as? GatewayError, code == "invalid_profile" {
                    profileIssue = CrewGatewayCopy.failure(error)
                    step = .profile
                    request(focus: .profile)
                } else {
                    issue = CrewGatewayCopy.failure(error)
                }
            case .success(let gateway):
                trusted = gateway
                record = CrewGatewayRecord(id: id, connectionName: connectionName, profile: profile, verifiedFingerprint: entered)
                step = .device
                checkStorage()
            }
        }
    }

    /// Request access again for a saved profile, for example after a lost
    /// approval reply or an expired request. Trust is re-established from the
    /// saved profile and recorded fingerprint, and an existing key is reused.
    func resume(_ saved: CrewGatewayRecord) {
        pending = true
        issue = nil
        connectionName = saved.connectionName
        deviceLabel = saved.deviceLabel ?? ""
        let attempt = attempt
        let profile = saved.publicProfile
        _Concurrency.Task {
            let result = await _Concurrency.Task.detached {
                Result { try GatewayProfile.trust(id: saved.id, profile: profile, verifiedFingerprint: saved.verifiedFingerprint) }
            }.value
            guard self.attempt == attempt else { return }
            pending = false
            switch result {
            case .failure(let error):
                step = .closed(CrewGatewayCopy.failure(error), mayHaveApproved: false)
            case .success(let gateway):
                trusted = gateway
                record = saved
                step = .device
                checkStorage()
            }
        }
    }

    // MARK: S03 Name this device

    func checkStorage() {
        guard let reference = record?.keyReference else { return }
        pending = true
        storageReady = false
        issue = nil
        let attempt = attempt
        _Concurrency.Task {
            do {
                let exists = try await vault.hasKey(reference: reference)
                guard self.attempt == attempt else { return }
                existingKey = exists
                storageReady = true
            } catch {
                guard self.attempt == attempt else { return }
                issue = Self.storage(error)
            }
            pending = false
        }
    }

    /// Creates this profile's device key unless one already exists (a lost
    /// approval reply reuses it), saves the public profile, then builds the
    /// disconnected gateway client for the explicit request.
    func continueWithDevice() {
        guard step == .device, storageReady, !pending, let trusted, var record, let reference = record.keyReference else { return }
        let label = deviceLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !label.isEmpty else {
            issue = "Enter a device name the host operator will recognize."
            request(focus: .deviceLabel)
            return
        }
        issue = nil
        pending = true
        record.deviceLabel = label
        let saved = record
        let attempt = attempt
        _Concurrency.Task {
            do {
                if !existingKey {
                    do { _ = try await vault.create(reference: reference) }
                    catch AppleDeviceKeyFailure.alreadyExists { existingKey = true }
                }
                guard self.attempt == attempt else { return }
                store.upsert(saved)
                self.record = saved
                try await model.installGateway(trusted, signer: CrewDeviceSigner(keys: vault, reference: reference))
                guard self.attempt == attempt else { return }
                step = .request
            } catch {
                guard self.attempt == attempt else { return }
                issue = error is AppleDeviceKeyFailure ? Self.storage(error) : CrewGatewayCopy.failure(error)
            }
            pending = false
        }
    }

    // MARK: S04/S05 Request access and compare

    func requestAccess() {
        guard step == .request, handle == nil, !pending else { return }
        issue = nil
        pending = true
        progress = "Requesting access"
        let attempt = attempt
        let observer = CrewSetupObserver { [weak self] event in self?.receive(event, attempt: attempt) }
        _Concurrency.Task {
            do {
                let handle = try await model.enroll(label: deviceLabel.trimmingCharacters(in: .whitespacesAndNewlines), observer: observer)
                guard self.attempt == attempt else { _ = handle.cancel(); return }
                // A terminal callback may already have arrived.
                if pending { self.handle = handle }
            } catch {
                // A throw means no attempt and no callback.
                guard self.attempt == attempt else { return }
                pending = false
                progress = nil
                issue = CrewGatewayCopy.failure(error)
            }
        }
    }

    private func receive(_ event: CrewSetupEvent, attempt: UUID) {
        guard self.attempt == attempt else { return }
        switch event {
        case .changed(let update):
            progress = CrewGatewayCopy.progress(update.progress)
            if case .awaitingApproval(_, let code, let expires) = update.progress {
                step = .compare(groups: CrewComparisonCode.groups(code), expiresAtMs: expires)
            }
        case .finished(let result):
            finishEnrollment()
            switch result.outcome {
            case .approved(let access):
                step = .granted(access)
                if let id = record?.id { model.connect(profileId: id) }
            case .canceled:
                step = .closed(CrewGatewayCopy.outcome(.canceled), mayHaveApproved: false)
            case .outcomeUnknown:
                step = .closed(CrewGatewayCopy.outcome(.outcomeUnknown), mayHaveApproved: true)
            case .failed(let reason):
                step = .request
                issue = reason.message
            }
        case .failed(let reason):
            finishEnrollment()
            step = .request
            issue = reason.message
        }
    }

    private func finishEnrollment() {
        handle = nil
        pending = false
        progress = nil
    }

    /// Cancel asks the connector to close the attempt; its one terminal
    /// callback still decides what is shown. Closing the UI is not proof
    /// that a concurrent host approval was undone.
    func cancelRequest() {
        guard let handle else { return }
        progress = "Closing the request"
        _ = handle.cancel()
    }

    /// Reconnect with the same key and verified profile to learn whether the
    /// host approved this device.
    func reconnectToCheck() {
        guard let id = record?.id else { return }
        model.connect(profileId: id)
    }

    /// Leaving setup fences late callbacks and cancels a live attempt.
    func close() {
        attempt = UUID()
        _ = handle?.cancel()
        handle = nil
        pending = false
        progress = nil
    }

    private func request(focus field: Field) { focusRequest = (field, UUID()) }

    static func storage(_ error: Error) -> String {
        switch error as? AppleDeviceKeyFailure {
        case .locked: return "Unlock this device's credential storage and try again."
        case .missing: return "This device's key for the profile is missing. Remove the profile and request access again."
        default: return "This device's secure storage is unavailable. Check device settings, then try again."
        }
    }
}

/// Connecting and removing saved gateway profiles (S01/S08). Trust is
/// re-established from the stored profile and the operator's recorded
/// fingerprint; the connector repeats every TLS and pin check.
@MainActor
enum CrewGatewayProfiles {
    static func connect(_ record: CrewGatewayRecord, model: CrewSessionModel, vault: AppleDeviceKeyVault = AppleDeviceKeyVault()) async throws {
        guard let reference = record.keyReference else { throw GatewayError.Failure(code: "invalid_profile", message: "The saved profile is invalid.") }
        let profile = record.publicProfile
        let gateway = try await _Concurrency.Task.detached {
            try GatewayProfile.trust(id: record.id, profile: profile, verifiedFingerprint: record.verifiedFingerprint)
        }.value
        try await model.installGateway(gateway, signer: CrewDeviceSigner(keys: vault, reference: reference))
        model.connect(profileId: record.id)
    }

    /// Removes the saved profile and retires this app's device key. Host
    /// access remains until it is revoked or expires at the host.
    static func remove(_ record: CrewGatewayRecord, model: CrewSessionModel, store: CrewGatewayStore = CrewGatewayStore(), vault: AppleDeviceKeyVault = AppleDeviceKeyVault()) async throws {
        if model.gatewayProfileId == record.id {
            try await model.retireDeviceAccess()
        } else if let reference = record.keyReference {
            try await vault.retire(reference: reference)
        }
        store.remove(record.id)
    }
}
