import CrewBindings
import Foundation
import XCTest
#if os(macOS)
@testable import CrewMac
#elseif os(iOS)
@testable import CrewIOS
#endif

/// Gateway host adapters with synthetic values only. Reply and transcript
/// objects are the generated classes' handle-free mocks; no key, network,
/// profile parsing or trust decision is exercised here.
final class CrewGatewayTests: XCTestCase {
    func testFingerprintEchoDropsConsoleSeparatorsAndGroupsTheEntry() {
        let echo = CrewFingerprint.echo(" AB:cd 01\n23\t")
        XCTAssertEqual(echo, "abcd0123")
        XCTAssertEqual(CrewFingerprint.groups(String(repeating: "0a1b", count: 16)).count, 16)
        XCTAssertEqual(CrewFingerprint.groups("0001000"), ["0001", "000"])
    }

    func testComparisonMarksEachDifferingGroupInText() {
        let profile = String(repeating: "abcd", count: 16)
        var entered = Array(profile)
        entered[17] = "0"
        let rows = CrewFingerprint.comparison(profile: profile, entered: String(entered))
        XCTAssertEqual(rows.count, 16)
        XCTAssertEqual(rows.filter(\.differs).map(\.index), [5])
        XCTAssertEqual(CrewFingerprint.comparison(profile: profile, entered: String(profile.prefix(60))).last?.differs, true)
    }

    func testComparisonCodeKeepsLeadingZeroesInFourGroups() {
        XCTAssertEqual(CrewComparisonCode.groups("000a0b0c00d0e0f0"), ["000a", "0b0c", "00d0", "e0f0"])
    }

    func testConnectorReasonsUseTheContractCopy() {
        XCTAssertEqual(CrewGatewayCopy.failure(GatewayError.Failure(code: "tls_trust", message: "x")), CrewGatewayCopy.trustFailure)
        XCTAssertEqual(CrewGatewayCopy.failure(GatewayError.Failure(code: "key_locked", message: "x")), "Unlock this device's credential storage and try again.")
        XCTAssertEqual(CrewGatewayCopy.failure(GatewayError.Failure(code: "transport_closed", message: "Connection closed.")), "Connection closed.")
        XCTAssertEqual(CrewGatewayCopy.outcome(.canceled), "Request closed.")
    }

    func testStoreKeepsOnlyPublicProfileFieldsAndRemovesById() throws {
        let suite = "crew.gateway-tests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = CrewGatewayStore(defaults: defaults)
        let profile = PublicProfile(endpointId: String(repeating: "1", count: 32), name: "synthetic host", url: "wss://host.test:7443/native/ws", tlsLeafSha256: String(repeating: "a", count: 64), trustPem: "-----BEGIN CERTIFICATE-----\nsynthetic\n-----END CERTIFICATE-----\n")
        let id = UUID().uuidString
        store.upsert(CrewGatewayRecord(id: id, connectionName: "Lab", profile: profile, verifiedFingerprint: String(repeating: "a", count: 64)))
        let saved = try XCTUnwrap(store.load().first)
        XCTAssertEqual(saved.publicProfile, profile)
        XCTAssertEqual(saved.keyReference, UUID(uuidString: id))
        let stored = try XCTUnwrap(defaults.data(forKey: CrewGatewayStore.key))
        let keys = try XCTUnwrap(JSONSerialization.jsonObject(with: stored) as? [[String: Any]]).first.map { Set($0.keys) }
        XCTAssertEqual(keys, ["id", "connectionName", "endpointId", "name", "url", "tlsLeafSha256", "trustPem", "verifiedFingerprint"])
        store.remove(id)
        XCTAssertTrue(store.load().isEmpty)
    }

    func testSignerCompletesEachReplyFromTheKeyServiceAndSkipsCanceledOnes() async {
        let keys = SyntheticKeys()
        let signer = CrewDeviceSigner(keys: keys, reference: UUID())
        let key = RecordingPublicKeyReply(pending: true)
        signer.publicKey(reply: key)
        let auth = RecordingSignatureReply(pending: true)
        signer.signAuth(transcript: SyntheticAuthTranscript(noHandle: .init()), reply: auth)
        let canceled = RecordingSignatureReply(pending: false)
        signer.signEnrollment(transcript: SyntheticEnrollTranscript(noHandle: .init()), reply: canceled)
        await fulfillment(of: [key.done, auth.done], timeout: 3)
        XCTAssertEqual(key.completed, Data(repeating: 1, count: 32))
        XCTAssertEqual(auth.completed, Data(repeating: 2, count: 64))
        XCTAssertNil(canceled.completed)
        XCTAssertNil(canceled.failed)
    }

    func testSignerReportsTypedKeyServiceFailures() async {
        let keys = SyntheticKeys(locked: true)
        let signer = CrewDeviceSigner(keys: keys, reference: UUID())
        let reply = RecordingSignatureReply(pending: true)
        signer.signAuth(transcript: SyntheticAuthTranscript(noHandle: .init()), reply: reply)
        await fulfillment(of: [reply.done], timeout: 3)
        XCTAssertEqual(reply.failed, .locked)
        XCTAssertNil(reply.completed)
    }
}

private struct SyntheticKeys: CrewDeviceKeys {
    var locked = false
    func devicePublicKey(reference: UUID) async throws -> Data {
        if locked { throw CrewSignerFailure(error: .locked) }
        return Data(repeating: 1, count: 32)
    }
    func signAuthentication(_ transcript: any AuthTranscriptProtocol, reference: UUID) async throws -> Data {
        if locked { throw CrewSignerFailure(error: .locked) }
        return Data(repeating: 2, count: 64)
    }
    func signEnrollment(_ transcript: any EnrollTranscriptProtocol, reference: UUID) async throws -> Data {
        if locked { throw CrewSignerFailure(error: .locked) }
        return Data(repeating: 3, count: 64)
    }
    func retireDeviceKey(reference: UUID) async throws {}
}

private final class SyntheticAuthTranscript: AuthTranscript, @unchecked Sendable {
    override func signingBytes() -> Data { Data("synthetic auth".utf8) }
}

private final class SyntheticEnrollTranscript: EnrollTranscript, @unchecked Sendable {
    override func signingBytes() -> Data { Data("synthetic enroll".utf8) }
}

private final class RecordingPublicKeyReply: PublicKeyReply, @unchecked Sendable {
    let done = XCTestExpectation(description: "public key reply")
    private let lock = NSLock()
    private let pending: Bool
    private var bytes: Data?
    var completed: Data? { lock.lock(); defer { lock.unlock() }; return bytes }
    init(pending: Bool) { self.pending = pending; super.init(noHandle: .init()) }
    required init(unsafeFromHandle handle: UInt64) { fatalError("Synthetic reply only") }
    override func isPending() -> Bool { pending }
    override func complete(bytes: Data) throws { lock.lock(); self.bytes = bytes; lock.unlock(); done.fulfill() }
    override func fail(error: SignerError) throws { done.fulfill() }
}

private final class RecordingSignatureReply: SignatureReply, @unchecked Sendable {
    let done = XCTestExpectation(description: "signature reply")
    private let lock = NSLock()
    private let pending: Bool
    private var bytes: Data?
    private var error: SignerError?
    var completed: Data? { lock.lock(); defer { lock.unlock() }; return bytes }
    var failed: SignerError? { lock.lock(); defer { lock.unlock() }; return error }
    init(pending: Bool) { self.pending = pending; super.init(noHandle: .init()) }
    required init(unsafeFromHandle handle: UInt64) { fatalError("Synthetic reply only") }
    override func isPending() -> Bool { pending }
    override func complete(bytes: Data) throws { lock.lock(); self.bytes = bytes; lock.unlock(); done.fulfill() }
    override func fail(error: SignerError) throws { lock.lock(); self.error = error; lock.unlock(); done.fulfill() }
}
