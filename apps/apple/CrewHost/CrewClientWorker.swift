import CrewBindings
import Foundation

/// Owns the real Rust facade. Synchronous FFI calls execute on this actor,
/// never on the main actor. Networking, clocks, validation and policy stay in
/// Rust; this type only serializes calls and disposes the current subscription.
actor CrewClientWorker {
    private var client: (any CrewClientProtocol)?
    private var subscription: UInt64?
    private var instance: UUID?

    #if os(macOS)
    func discoverLocal(observer: any ClientObserver, instance: UUID) throws -> [LocalProfileInfo] {
        try install(CrewClient.local(), observer: observer, instance: instance)
        return try requireClient(instance).localProfiles()
    }
    #endif

    /// Gateway composition installs its real generated factory result here.
    /// The factory, TLS verification and typed signer belong to the bridge.
    func install(_ replacement: any CrewClientProtocol, observer: any ClientObserver, instance: UUID) throws {
        disposeCurrent()
        do {
            subscription = try replacement.observe(observer: observer)
            client = replacement
            self.instance = instance
        } catch {
            try? replacement.close()
            throw error
        }
    }

    /// Builds the scoped gateway client from a trusted profile and the typed
    /// signer, then installs it. The factory blocks while the signer reads the
    /// device public key (bounded in Rust), so it runs here, never on main.
    /// The returned client is disconnected until `connect(profileId:)`.
    func installGateway(_ profile: GatewayProfile, signer: any DeviceSigner, observer: any ClientObserver, instance: UUID) throws {
        try _Concurrency.Task.checkCancellation()
        try install(CrewClient.gateway(profile: profile, signer: signer), observer: observer, instance: instance)
    }

    /// One explicit enrollment attempt on the installed gateway client. A
    /// throw means no attempt and no callback; a handle means exactly one
    /// terminal callback on the observer.
    func enroll(label: String, observer: any SetupObserver, instance: UUID) throws -> EnrollmentHandle {
        try _Concurrency.Task.checkCancellation()
        return try requireClient(instance).enroll(label: label, observer: observer)
    }

    /// Local retirement of this app's device key through the signer; the
    /// client closes and host access remains until revoked or expired.
    func retireDeviceAccess(instance: UUID) throws {
        let client = try requireClient(instance)
        defer { disposeCurrent() }
        try client.retireDeviceAccess()
    }

    func connect(profileId: String, instance: UUID) throws {
        try _Concurrency.Task.checkCancellation()
        try requireClient(instance).connect(profileId: profileId)
    }

    func selectWorkspace(_ id: String, instance: UUID) throws {
        try _Concurrency.Task.checkCancellation()
        try requireClient(instance).selectWorkspace(workspaceId: id)
    }

    func read(_ queries: [ViewQuery], instance: UUID) throws -> CrewReadBatch {
        try _Concurrency.Task.checkCancellation()
        let client = try requireClient(instance)
        return CrewReadBatch(
            context: try client.connectionContext(),
            workspaces: try client.workspaces(),
            views: try queries.map { CrewQueriedView(query: $0, data: try client.query(query: $0)) }
        )
    }

    /// Read-only Core availability for each captured intent. A malformed
    /// intent fails only its own entry, with the same error `plan` returns;
    /// a closed client fails the whole read. Nothing here plans or dispatches.
    func availability(_ intents: [ActionIntent], instance: UUID) throws -> [CrewAvailabilityRead] {
        try _Concurrency.Task.checkCancellation()
        let client = try requireClient(instance)
        return try intents.map { intent in
            do {
                let data = try client.query(query: .availability(intent: intent))
                guard case .availability(let availability) = data.payload, data.scope == intent.scope else {
                    return CrewAvailabilityRead(intent: intent, result: .failed(.DecodeFailure(message: "availability view")))
                }
                return CrewAvailabilityRead(intent: intent, result: .answered(availability))
            } catch ClientError.ClosedClient {
                throw ClientError.ClosedClient
            } catch let error as ClientError {
                return CrewAvailabilityRead(intent: intent, result: .failed(error))
            }
        }
    }

    func plan(_ intent: ActionIntent, instance: UUID) throws -> ActionPlan {
        try _Concurrency.Task.checkCancellation()
        return try requireClient(instance).plan(intent: intent)
    }

    func confirm(_ id: UInt64, instance: UUID) throws {
        try _Concurrency.Task.checkCancellation()
        try requireClient(instance).confirm(confirmationId: id)
    }

    func cancelConfirmation(_ id: UInt64, instance: UUID) {
        try? requireClient(instance).cancelConfirmation(confirmationId: id)
    }

    func setActive(_ active: Bool, instance: UUID) throws { try requireClient(instance).setActive(active: active) }
    func reconnect(instance: UUID) throws { try requireClient(instance).reconnect() }
    func disconnect(instance: UUID) throws { try requireClient(instance).disconnect() }
    func questionInfo(_ message: Message, instance: UUID) throws -> QuestionInfo {
        _ = try requireClient(instance)
        return try messageQuestionInfo(message: message)
    }

    func close(instance: UUID) {
        guard self.instance == instance else { return }
        disposeCurrent()
    }

    private func disposeCurrent() {
        let previous = client
        let previousSubscription = subscription
        client = nil
        subscription = nil
        instance = nil
        if let previousSubscription { try? previous?.removeObserver(subscriptionId: previousSubscription) }
        try? previous?.close()
    }

    private func requireClient(_ instance: UUID) throws -> any CrewClientProtocol {
        guard self.instance == instance, let client else { throw ClientError.ClosedClient }
        return client
    }
}

/// A read batch groups actual generated values; it is not a domain model or a
/// second reducer. Query identity lets the view cache apply only its own result.
struct CrewQueriedView: Sendable {
    let query: ViewQuery
    let data: ViewData
}

struct CrewReadBatch: Sendable {
    let context: ConnectionContext?
    let workspaces: Workspaces?
    let views: [CrewQueriedView]
}

/// The Rust dispatcher supplies ordered, bounded callbacks outside its locks.
/// A synchronous, short UI-executor handoff keeps that bound effective instead
/// of creating an unbounded Task per chunk. UI handlers never call FFI or wait
/// for the worker. Removing/closing a subscription need not wait for this call.
final class CrewUIObserver: ClientObserver, @unchecked Sendable {
    private let noticeHandler: @MainActor @Sendable (ClientNotice) -> Void
    private let terminalHandler: @MainActor @Sendable (TerminalChunk) -> Void

    init(
        notice: @escaping @MainActor @Sendable (ClientNotice) -> Void,
        terminal: @escaping @MainActor @Sendable (TerminalChunk) -> Void
    ) {
        noticeHandler = notice
        terminalHandler = terminal
    }

    func changed(notice: ClientNotice) {
        handoff { self.noticeHandler(notice) }
    }

    func terminal(chunk: TerminalChunk) {
        handoff { self.terminalHandler(chunk) }
    }

    private func handoff(_ operation: @MainActor @Sendable () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated { operation() }
        } else {
            DispatchQueue.main.sync { MainActor.assumeIsolated { operation() } }
        }
    }
}

/// One availability answer for exactly the intent that was asked about.
struct CrewAvailabilityRead: Sendable {
    enum Result: Equatable, Sendable {
        case answered(ActionAvailability)
        case failed(ClientError)
    }

    let intent: ActionIntent
    let result: Result
}

extension ActionIntent {
    /// The captured scope every intent carries; read only to fence results.
    var scope: ClientScope {
        switch self {
        case .sendMessage(let scope, _, _, _), .answer(let scope, _, _), .agent(let scope, _, _),
             .setModel(let scope, _, _, _, _), .agentInput(let scope, _, _, _),
             .resumeLimited(let scope, _), .stopTeam(let scope), .read(let scope, _),
             .attach(let scope, _, _, _), .attachInput(let scope, _, _),
             .attachResize(let scope, _, _, _), .detach(let scope, _):
            return scope
        }
    }
}
