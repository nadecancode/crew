import CrewBindings
import Foundation
import XCTest
#if os(macOS)
@testable import CrewMac
#else
@testable import CrewIOS
#endif

/// These native test sources are for the Apple runner. The fake implements the
/// actual generated protocol; production composition always installs Rust's
/// real factory. No fake handle, fabricated binding, network or key is used.
@MainActor
final class CrewClientWorkerTests: XCTestCase {
    func testWorkerCallsDoNotExecuteOnMainAndReadActualProjections() async throws {
        let worker = CrewClientWorker()
        let client = RecordingCrewClient()
        let instance = UUID()
        let observer = CrewUIObserver(notice: { _ in }, terminal: { _ in })
        try await worker.install(client, observer: observer, instance: instance)
        try await worker.connect(profileId: "synthetic-opaque-profile", instance: instance)
        let batch = try await worker.read([.team], instance: instance)
        XCTAssertEqual(batch.views.first?.query, .team)
        XCTAssertEqual(batch.views.first?.data.scope, CrewTestFixtures.scope)
        XCTAssertFalse(client.calledOnMain)
        XCTAssertEqual(client.calls, ["observe", "connect", "context", "workspaces", "query"])
        await worker.close(instance: instance)
        XCTAssertEqual(Array(client.calls.suffix(2)), ["remove-observer", "close"])
        XCTAssertFalse(client.calls.contains("stop-team"))
    }

    func testRetiredInstanceCannotCancelOrCloseReplacement() async throws {
        let worker = CrewClientWorker()
        let first = RecordingCrewClient()
        let second = RecordingCrewClient()
        let old = UUID()
        let current = UUID()
        let observer = CrewUIObserver(notice: { _ in }, terminal: { _ in })
        try await worker.install(first, observer: observer, instance: old)
        try await worker.install(second, observer: observer, instance: current)
        await worker.cancelConfirmation(7, instance: old)
        await worker.close(instance: old)
        do {
            _ = try await worker.plan(.stopTeam(scope: CrewTestFixtures.scope), instance: old)
            XCTFail("Retired instance must reject before reaching replacement")
        } catch { XCTAssertEqual(error as? ClientError, .ClosedClient) }
        XCTAssertEqual(second.calls, ["observe"])
        await worker.close(instance: current)
    }

    func testMainThreadCallbackRunsInlineAndPreservesTerminalEndOrder() {
        let received = ReceivedTerminalChunks()
        let observer = CrewUIObserver(notice: { _ in }, terminal: { received.chunks.append($0) })
        let first = TerminalChunk(scope: CrewTestFixtures.scope, attachmentId: 3, data: Data([65]), ended: nil)
        let final = TerminalChunk(scope: CrewTestFixtures.scope, attachmentId: 3, data: Data([66]), ended: .normal)
        observer.terminal(chunk: first)
        observer.terminal(chunk: final)
        XCTAssertEqual(received.chunks, [first, final])
    }

    func testWorkerCallbackWaitsForUIHandoffInsteadOfQueuingUnboundedJobs() async {
        let started = DispatchSemaphore(value: 0)
        let delivered = expectation(description: "delivered on main")
        let returned = expectation(description: "callback returned after UI delivery")
        let flag = LockedFlag()
        let observer = CrewUIObserver(notice: { _ in
            XCTAssertTrue(Thread.isMainThread)
            XCTAssertFalse(flag.value)
            delivered.fulfill()
        }, terminal: { _ in })
        let notice = CrewTestFixtures.notice
        DispatchQueue.global().async {
            started.signal()
            observer.changed(notice: notice)
            flag.set()
            returned.fulfill()
        }
        // Main is occupied until this synchronous section ends. The callback
        // cannot complete on its producer, so Rust's queue retains backpressure.
        XCTAssertEqual(started.wait(timeout: .now() + 2), .success)
        XCTAssertFalse(flag.value)
        await fulfillment(of: [delivered, returned], timeout: 3, enforceOrder: true)
        XCTAssertTrue(flag.value)
    }

    func testAlreadyRunningCallbackCannotPublishAfterModelClose() async throws {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let captured = try XCTUnwrap(client.observer)
        await model.close()
        captured.changed(notice: CrewTestFixtures.notice)
        captured.terminal(chunk: TerminalChunk(scope: CrewTestFixtures.scope, attachmentId: 3, data: Data([65]), ended: nil))
        XCTAssertNil(model.notice)
        XCTAssertNil(model.terminal)
        XCTAssertEqual(Array(client.calls.suffix(2)), ["remove-observer", "close"])
    }

}

private enum CrewTestFixtures {
    static let scope = ClientScope(endpointId: "synthetic-endpoint", workspaceId: "synthetic-workspace", generation: 2, daemonPid: 123, daemonStartedAtMs: 1_700_000_000_000, attempt: 4)
    static let notice = ClientNotice(scope: scope, revision: 1, changes: ChangeSet(revision: 1, agents: [], tasks: [], removed: [], session: false, questions: false, messages: false, events: false, usage: false, availability: false, pending: false, reset: false), endpoint: .authenticated, workspace: .live, reason: nil, outcomes: [])
}

@MainActor
private final class ReceivedTerminalChunks { var chunks: [TerminalChunk] = [] }

private final class LockedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var flag = false
    var value: Bool { lock.lock(); defer { lock.unlock() }; return flag }
    func set() { lock.lock(); defer { lock.unlock() }; flag = true }
}

private final class RecordingCrewClient: CrewClientProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedCalls: [String] = []
    private var main = false
    private var storedObserver: (any ClientObserver)?
    var calls: [String] { lock.lock(); defer { lock.unlock() }; return recordedCalls }
    var calledOnMain: Bool { lock.lock(); defer { lock.unlock() }; return main }
    var observer: (any ClientObserver)? { lock.lock(); defer { lock.unlock() }; return storedObserver }

    private func record(_ call: String) {
        lock.lock(); defer { lock.unlock() }
        recordedCalls.append(call)
        main = main || Thread.isMainThread
    }
    func observe(observer: any ClientObserver) throws -> UInt64 {
        record("observe")
        lock.lock(); defer { lock.unlock() }
        storedObserver = observer
        return 1
    }
    func removeObserver(subscriptionId: UInt64) throws {
        record("remove-observer")
        lock.lock(); defer { lock.unlock() }
        storedObserver = nil
    }
    func close() throws { record("close") }
    func connect(profileId: String) throws { record("connect") }
    func localProfiles() throws -> [LocalProfileInfo] { record("profiles"); return [] }
    func connectionContext() throws -> ConnectionContext? { record("context"); return nil }
    func workspaces() throws -> Workspaces? { record("workspaces"); return nil }
    func query(query: ViewQuery) throws -> ViewData {
        record("query")
        return ViewData(scope: CrewTestFixtures.scope, revision: 1, synchronizedAtMs: nil, freshness: .fresh, payload: .team(TeamData(agents: [], relations: [])))
    }
    func plan(intent: ActionIntent) throws -> ActionPlan { record("plan"); return .dispatch(operationId: 1) }
    func confirm(confirmationId: UInt64) throws { record("confirm") }
    func cancelConfirmation(confirmationId: UInt64) throws { record("cancel") }
    func disconnect() throws { record("disconnect") }
    func reconnect() throws { record("reconnect") }
    func selectWorkspace(workspaceId: String) throws { record("select") }
    func setActive(active: Bool) throws { record("active") }
}
