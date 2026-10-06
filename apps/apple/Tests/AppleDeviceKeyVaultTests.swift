import CryptoKit
import Foundation
import XCTest
#if os(macOS)
@testable import CrewMac
#else
@testable import CrewIOS
#endif

/// Synthetic in-memory seeds only. Actual Apple Keychain behavior, signing,
/// locking and entitlement checks are deferred to the operator's Apple runner.
final class AppleDeviceKeyVaultTests: XCTestCase {
    func testCreatedIdentityReturnsPublicBytesAndCanBeReadAgain() async throws {
        let storage = SyntheticDeviceSeedStorage()
        let vault = AppleDeviceKeyVault(storage: storage)
        let reference = UUID()
        let publicKey = try await vault.create(reference: reference)
        let seed = try storage.read(reference: reference)
        XCTAssertEqual(publicKey.count, 32)
        XCTAssertEqual(seed.count, 32)
        XCTAssertNotEqual(publicKey, seed)
        let restored = try await vault.publicKey(reference: reference)
        XCTAssertEqual(restored, publicKey)
    }

    func testMissingIdentityDoesNotSilentlyCreateAnotherKey() async throws {
        let storage = SyntheticDeviceSeedStorage()
        let vault = AppleDeviceKeyVault(storage: storage)
        let reference = UUID()
        do {
            _ = try await vault.publicKey(reference: reference)
            XCTFail("Missing identity must require explicit recovery")
        } catch {
            XCTAssertEqual(error as? AppleDeviceKeyFailure, .missing)
        }
        XCTAssertEqual(storage.count, 0)
    }

    func testCorruptSeedDoesNotBecomeAnIdentity() async throws {
        let storage = SyntheticDeviceSeedStorage()
        let reference = UUID()
        try storage.insertNew(Data(repeating: 0x11, count: 31), reference: reference)
        let vault = AppleDeviceKeyVault(storage: storage)
        do {
            _ = try await vault.publicKey(reference: reference)
            XCTFail("Invalid seed must be rejected")
        } catch {
            XCTAssertEqual(error as? AppleDeviceKeyFailure, .invalidStoredSeed)
        }
    }

    func testExplicitCreateDoesNotReplaceAnExistingIdentity() async throws {
        let storage = SyntheticDeviceSeedStorage()
        let reference = UUID()
        let seed = Data(repeating: 0x11, count: 32)
        try storage.insertNew(seed, reference: reference)
        let expected = try Curve25519.Signing.PrivateKey(rawRepresentation: seed).publicKey.rawRepresentation
        let vault = AppleDeviceKeyVault(storage: storage)
        do {
            _ = try await vault.create(reference: reference)
            XCTFail("Duplicate creation must preserve the existing key")
        } catch {
            XCTAssertEqual(error as? AppleDeviceKeyFailure, .alreadyExists)
        }
        let current = try await vault.publicKey(reference: reference)
        XCTAssertEqual(current, expected)
    }

    func testRetirementRemovesOnlyItsOwnIdentity() async throws {
        let storage = SyntheticDeviceSeedStorage()
        let vault = AppleDeviceKeyVault(storage: storage)
        let removed = UUID()
        let retained = UUID()
        _ = try await vault.create(reference: removed)
        let retainedPublic = try await vault.create(reference: retained)
        try await vault.retire(reference: removed)
        try await vault.retire(reference: removed)
        XCTAssertEqual(storage.count, 1)
        let current = try await vault.publicKey(reference: retained)
        XCTAssertEqual(current, retainedPublic)
    }
}

/// The lock protects every dictionary access; the vault's actor serializes
/// its callers. This test store is never linked into a production app target.
private final class SyntheticDeviceSeedStorage: AppleDeviceSeedStorage, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [UUID: Data] = [:]

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return values.count
    }

    func insertNew(_ seed: Data, reference: UUID) throws {
        lock.lock()
        defer { lock.unlock() }
        guard values[reference] == nil else { throw AppleDeviceKeyFailure.alreadyExists }
        values[reference] = seed
    }

    func read(reference: UUID) throws -> Data {
        lock.lock()
        defer { lock.unlock() }
        guard let seed = values[reference] else { throw AppleDeviceKeyFailure.missing }
        return seed
    }

    func remove(reference: UUID) throws {
        lock.lock()
        defer { lock.unlock() }
        values.removeValue(forKey: reference)
    }
}
