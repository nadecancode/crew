import CrewBindings
import CryptoKit
import Foundation
import Security

/// Host-only storage errors. No key data, account identifier or profile is
/// included in an error description or a domain/FFI value.
enum AppleDeviceKeyFailure: Error, Equatable {
    case missing
    case alreadyExists
    case locked
    case invalidStoredSeed
    case unavailable(OSStatus)
}

/// This port is confined to CrewHost and its isolated tests. Private seeds
/// travel only between the OS store and the crypto helper, never a view, the
/// Rust domain model, a profile, preferences, logging, or an FFI callback.
protocol AppleDeviceSeedStorage: Sendable {
    func insertNew(_ seed: Data, reference: UUID) throws
    func read(reference: UUID) throws -> Data
    func remove(reference: UUID) throws
}

struct KeychainDeviceSeedStorage: AppleDeviceSeedStorage {
    private let service: String

    init(service: String = "dev.crew.Crew.native-device-key") {
        self.service = service
    }

    func insertNew(_ seed: Data, reference: UUID) throws {
        var attributes = query(reference: reference)
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        attributes[kSecValueData as String] = seed
        try check(SecItemAdd(attributes as CFDictionary, nil))
    }

    func read(reference: UUID) throws -> Data {
        var attributes = query(reference: reference)
        attributes[kSecMatchLimit as String] = kSecMatchLimitOne
        attributes[kSecReturnData as String] = true
        // A locked store is an explicit connector outcome. Do not open an OS
        // authentication prompt after a short transport deadline has started.
        attributes[kSecUseAuthenticationUI as String] = kSecUseAuthenticationUIFail
        var result: CFTypeRef?
        try check(SecItemCopyMatching(attributes as CFDictionary, &result))
        guard let seed = result as? Data else { throw AppleDeviceKeyFailure.invalidStoredSeed }
        return seed
    }

    func remove(reference: UUID) throws {
        let status = SecItemDelete(query(reference: reference) as CFDictionary)
        if status != errSecItemNotFound { try check(status) }
    }

    private func query(reference: UUID) -> [String: Any] {
        var attributes: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: reference.uuidString,
            kSecAttrSynchronizable as String: false
        ]
        #if os(macOS)
        // The legacy Mac keychain does not enforce the iOS accessibility
        // classes. Use the data-protection keychain and report any unavailable
        // entitlement/store explicitly, without a less protected fallback.
        attributes[kSecUseDataProtectionKeychain as String] = true
        #endif
        return attributes
    }

    private func check(_ status: OSStatus) throws {
        switch status {
        case errSecSuccess: return
        case errSecItemNotFound: throw AppleDeviceKeyFailure.missing
        case errSecDuplicateItem: throw AppleDeviceKeyFailure.alreadyExists
        case errSecInteractionNotAllowed, errSecAuthFailed: throw AppleDeviceKeyFailure.locked
        default: throw AppleDeviceKeyFailure.unavailable(status)
        }
    }
}

/// Ed25519 device identity operations. CryptoKit uses OS entropy for a new
/// private key. Only the public 32 bytes leave its API; its private 32-byte
/// seed is stored with device-only, nonsynchronizing, unlocked accessibility.
///
/// There is deliberately no untyped `sign(Data)` entry point. Signing accepts
/// only the Rust-constructed AuthTranscript/EnrollTranscript, through the
/// `CrewDeviceSigner` callback adapter, and never exposes a store reference or
/// private key through UniFFI.
actor AppleDeviceKeyVault {
    private let storage: any AppleDeviceSeedStorage

    init(storage: any AppleDeviceSeedStorage = KeychainDeviceSeedStorage()) {
        self.storage = storage
    }

    func create(reference: UUID) throws -> Data {
        let key = Curve25519.Signing.PrivateKey()
        try storage.insertNew(key.rawRepresentation, reference: reference)
        return key.publicKey.rawRepresentation
    }

    func publicKey(reference: UUID) throws -> Data {
        try loadPrivateKey(reference: reference).publicKey.rawRepresentation
    }

    func retire(reference: UUID) throws {
        try storage.remove(reference: reference)
    }

    private func loadPrivateKey(reference: UUID) throws -> Curve25519.Signing.PrivateKey {
        let seed = try storage.read(reference: reference)
        guard seed.count == 32 else { throw AppleDeviceKeyFailure.invalidStoredSeed }
        do {
            return try Curve25519.Signing.PrivateKey(rawRepresentation: seed)
        } catch {
            throw AppleDeviceKeyFailure.invalidStoredSeed
        }
    }
}

/// The gateway signer's key service. Storage failures become the connector's
/// typed signer errors; no key, store reference or OS status crosses over.
extension AppleDeviceKeyVault: CrewDeviceKeys {
    func devicePublicKey(reference: UUID) throws -> Data {
        try Self.typed { try publicKey(reference: reference) }
    }

    func signAuthentication(_ transcript: any AuthTranscriptProtocol, reference: UUID) throws -> Data {
        try Self.typed { try loadPrivateKey(reference: reference).signature(for: transcript.signingBytes()) }
    }

    func signEnrollment(_ transcript: any EnrollTranscriptProtocol, reference: UUID) throws -> Data {
        try Self.typed { try loadPrivateKey(reference: reference).signature(for: transcript.signingBytes()) }
    }

    func retireDeviceKey(reference: UUID) throws {
        try Self.typed { try retire(reference: reference) }
    }

    /// Whether this profile already has a device key: an existing key after a
    /// lost approval reply is reused rather than replaced.
    func hasKey(reference: UUID) throws -> Bool {
        do {
            _ = try publicKey(reference: reference)
            return true
        } catch AppleDeviceKeyFailure.missing {
            return false
        }
    }

    private static func typed<T>(_ operation: () throws -> T) throws -> T {
        do { return try operation() } catch {
            switch error as? AppleDeviceKeyFailure {
            case .locked: throw CrewSignerFailure(error: .locked)
            case .missing, .alreadyExists, .invalidStoredSeed, .unavailable, nil: throw CrewSignerFailure(error: .unavailable)
            }
        }
    }
}
