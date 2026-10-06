import CrewBindings
import CrewUI
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

    func testDraftVisibleAfterOrdinaryReconnectAndPurgedAfterRevocation() async throws {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let observer = try XCTUnwrap(client.observer)
        observer.changed(notice: CrewTestFixtures.notice)
        let original = CrewDraftKey(scope: CrewTestFixtures.scope, purpose: "message:synthetic-agent")
        model.sendDraft(original).wrappedValue = "retained message text"
        let current = ClientScope(endpointId: original.scope.endpointId, workspaceId: original.scope.workspaceId, generation: original.scope.generation, daemonPid: original.scope.daemonPid, daemonStartedAtMs: original.scope.daemonStartedAtMs, attempt: original.scope.attempt + 1)
        observer.changed(notice: ClientNotice(scope: current, revision: 2, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .live, reason: nil, outcomes: []))
        let reopened = CrewDraftKey(scope: current, purpose: original.purpose)
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "retained message text")
        observer.changed(notice: ClientNotice(scope: current, revision: 3, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .accessRevoked, reason: nil, outcomes: []))
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "")
        await model.close()
        XCTAssertFalse(client.calls.contains("plan"))
    }

    /// Dispatch, connection loss, new verified attempt, reopened composer and
    /// the Core's replayed final notice: original operation ID, new scope.
    func testHeldSendCompletesWithNewAttemptScopeAndKeepsLaterEdits() async throws {
        let (model, client, observer, original, current) = try await heldSend()
        let reopened = CrewDraftKey(scope: current, purpose: original.purpose)
        XCTAssertTrue(model.sendPending(reopened))
        XCTAssertEqual(model.sendReceipt(reopened), CrewSendReceipt(recipient: "synthetic-agent", body: "pending identified message", state: .held))
        model.sendDraft(reopened).wrappedValue = "later edit"
        XCTAssertEqual(model.sendReceipt(reopened)?.body, "pending identified message")
        observer.changed(notice: live(current, revision: 3, outcomes: [OperationNotice(scope: current, operationId: 1, attachmentId: nil, kind: .success, reason: nil)]))
        XCTAssertFalse(model.sendPending(reopened))
        XCTAssertNil(model.sendReceipt(reopened))
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "later edit")
        XCTAssertEqual(client.calls.filter { $0 == "plan" }.count, 1)
        await model.close()
    }

    func testHeldSendUnknownKeepsBodyAndExplanation() async throws {
        let (model, client, observer, original, current) = try await heldSend()
        let reopened = CrewDraftKey(scope: current, purpose: original.purpose)
        let unknown = OperationNotice(scope: current, operationId: 1, attachmentId: nil, kind: .unknown, reason: nil)
        observer.changed(notice: live(current, revision: 3, outcomes: [unknown]))
        XCTAssertFalse(model.sendPending(reopened))
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "pending identified message")
        XCTAssertEqual(model.sendReceipt(reopened)?.state, .outcome(unknown))
        XCTAssertEqual(client.calls.filter { $0 == "plan" }.count, 1)
        await model.close()
    }

    func testRevocationPurgesHeldReceiptAndIgnoresLaterNotice() async throws {
        let (model, client, observer, original, current) = try await heldSend()
        let reopened = CrewDraftKey(scope: current, purpose: original.purpose)
        observer.changed(notice: ClientNotice(scope: current, revision: 3, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .accessRevoked, reason: nil, outcomes: []))
        XCTAssertFalse(model.sendPending(reopened))
        XCTAssertNil(model.sendReceipt(reopened))
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "")
        observer.changed(notice: live(current, revision: 4, outcomes: [OperationNotice(scope: current, operationId: 1, attachmentId: nil, kind: .success, reason: nil)]))
        XCTAssertNil(model.sendReceipt(reopened))
        XCTAssertEqual(client.calls.filter { $0 == "plan" }.count, 1)
        await model.close()
    }

    /// Send A, edit B, replace the client: A may already be delivered, so its
    /// receipt stays visible as unknown; B is untouched and nothing is sent.
    func testClientReplacementRetiresHeldReceiptAsUnknown() async throws {
        let (model, _, _, original, current) = try await heldSend()
        let reopened = CrewDraftKey(scope: current, purpose: original.purpose)
        model.sendDraft(reopened).wrappedValue = "message B"
        let untouched = CrewDraftKey(scope: current, purpose: "message:other-agent")
        model.sendDraft(untouched).wrappedValue = "never submitted"
        let replacement = RecordingCrewClient()
        try await model.install(replacement)
        XCTAssertFalse(model.sendPending(reopened))
        XCTAssertEqual(model.sendReceipt(reopened), CrewSendReceipt(recipient: "synthetic-agent", body: "pending identified message", state: .retired))
        XCTAssertEqual(model.sendDraft(reopened).wrappedValue, "message B")
        XCTAssertNil(model.sendExplanation(reopened))
        XCTAssertNil(model.sendReceipt(untouched))
        XCTAssertNil(model.sendExplanation(untouched))
        XCTAssertFalse(replacement.calls.contains("plan"))
        await model.close()
    }

    /// Controls read the Core's answer for their exact intent. A malformed
    /// intent fails only itself; nothing is planned or dispatched.
    func testAvailabilityUsesTheCoreAnswerForTheExactIntent() async throws {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let observer = try XCTUnwrap(client.observer)
        observer.changed(notice: CrewTestFixtures.notice)
        let scope = CrewTestFixtures.scope
        let stop = ActionIntent.stopTeam(scope: scope)
        let resume = ActionIntent.resumeLimited(scope: scope, restart: false)
        let empty = ActionIntent.sendMessage(scope: scope, to: "synthetic-agent", body: "", messageId: "synthetic-id")
        let entries = [CrewWatchedIntent(control: "stop", intent: stop), CrewWatchedIntent(control: "resume", intent: resume), CrewWatchedIntent(control: "send", intent: empty)]
        XCTAssertEqual(model.availability(stop, control: "stop"), .checking)
        model.watchAvailability(entries)
        await model.waitForAvailability()
        XCTAssertEqual(model.availability(stop, control: "stop"), CrewControlAvailability(available: false, reason: "Synthetic Core reason"))
        XCTAssertEqual(model.availability(resume, control: "resume"), CrewControlAvailability(available: true, reason: nil))
        XCTAssertEqual(model.availability(empty, control: "send"), CrewControlAvailability(available: false, reason: "body: must not be empty"))
        XCTAssertFalse(client.calls.contains("plan"))

        // A shared change re-reads every watched intent.
        let before = client.calls.filter { $0 == "availability" }.count
        observer.changed(notice: ClientNotice(scope: scope, revision: 2, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .live, reason: nil, outcomes: []))
        await model.waitForAvailability()
        XCTAssertEqual(client.calls.filter { $0 == "availability" }.count, before + 3)

        // An intent captured under an earlier attempt is never asked about.
        let current = ClientScope(endpointId: scope.endpointId, workspaceId: scope.workspaceId, generation: scope.generation, daemonPid: scope.daemonPid, daemonStartedAtMs: scope.daemonStartedAtMs, attempt: scope.attempt + 1)
        observer.changed(notice: ClientNotice(scope: current, revision: 3, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .live, reason: nil, outcomes: []))
        await model.waitForAvailability()
        XCTAssertEqual(model.availability(stop, control: "stop"), CrewControlAvailability(available: false, reason: "Reopen this target from the current workspace connection."))
        XCTAssertEqual(client.calls.filter { $0 == "availability" }.count, before + 3)
        model.unwatchAvailability(entries)
        XCTAssertFalse(client.calls.contains("plan"))
        await model.close()
    }

    /// While edited input is being checked, a control keeps its previous
    /// answer instead of flashing disabled; a vanished control forgets it.
    func testEditedInputKeepsTheControlsLastAnswerUntilChecked() async throws {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let observer = try XCTUnwrap(client.observer)
        observer.changed(notice: CrewTestFixtures.notice)
        let scope = CrewTestFixtures.scope
        let first = CrewWatchedIntent(control: "send", intent: .sendMessage(scope: scope, to: "synthetic-agent", body: "a", messageId: "synthetic-id"))
        // The recording client rejects an empty body, so the edited answer
        // differs from the retained one.
        let edited = CrewWatchedIntent(control: "send", intent: .sendMessage(scope: scope, to: "synthetic-agent", body: "", messageId: "synthetic-id"))
        model.watchAvailability([first])
        await model.waitForAvailability()
        let answered = CrewControlAvailability(available: true, reason: nil)
        XCTAssertEqual(model.availability(first.intent, control: "send"), answered)
        // The view modifier watches the new entry before releasing the old.
        model.watchAvailability([edited])
        model.unwatchAvailability([first])
        // The read runs on the main actor, so it cannot land before this await.
        XCTAssertEqual(model.availability(edited.intent, control: "send"), answered)
        await model.waitForAvailability()
        XCTAssertEqual(model.availability(edited.intent, control: "send"), CrewControlAvailability(available: false, reason: "body: must not be empty"))
        model.unwatchAvailability([edited])
        XCTAssertEqual(model.availability(edited.intent, control: "send"), .checking)
        await model.close()
    }

    /// The composer's availability intent is the one Send dispatches.
    func testSendAvailabilityIntentMatchesTheDispatchedIntent() async throws {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let observer = try XCTUnwrap(client.observer)
        observer.changed(notice: CrewTestFixtures.notice)
        let key = CrewDraftKey(scope: CrewTestFixtures.scope, purpose: "message:synthetic-agent")
        model.prepareSendDraft(key)
        model.sendDraft(key).wrappedValue = "exact text"
        let queried = try XCTUnwrap(model.sendIntent(key, to: "synthetic-agent"))
        model.send(key, to: "synthetic-agent")
        await model.waitForSubmissionBatch()
        XCTAssertEqual(client.planned, [queried])
        await model.close()
    }

    private func heldSend() async throws -> (CrewSessionModel, RecordingCrewClient, any ClientObserver, CrewDraftKey, ClientScope) {
        let model = CrewSessionModel()
        let client = RecordingCrewClient()
        try await model.install(client)
        let observer = try XCTUnwrap(client.observer)
        let originalScope = CrewTestFixtures.scope
        observer.changed(notice: CrewTestFixtures.notice)
        let original = CrewDraftKey(scope: originalScope, purpose: "message:synthetic-agent")
        model.sendDraft(original).wrappedValue = "pending identified message"
        model.send(original, to: "synthetic-agent")
        await model.waitForSubmissionBatch()
        XCTAssertEqual(client.calls.filter { $0 == "plan" }.count, 1)
        let current = ClientScope(endpointId: originalScope.endpointId, workspaceId: originalScope.workspaceId, generation: originalScope.generation, daemonPid: originalScope.daemonPid, daemonStartedAtMs: originalScope.daemonStartedAtMs, attempt: originalScope.attempt + 1)
        observer.changed(notice: ClientNotice(scope: current, revision: 2, changes: CrewTestFixtures.notice.changes, endpoint: .reconnecting, workspace: .reconnecting, reason: nil, outcomes: []))
        return (model, client, observer, original, current)
    }

    private func live(_ scope: ClientScope, revision: UInt64, outcomes: [OperationNotice]) -> ClientNotice {
        ClientNotice(scope: scope, revision: revision, changes: CrewTestFixtures.notice.changes, endpoint: .authenticated, workspace: .live, reason: nil, outcomes: outcomes)
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
    private var plannedIntents: [ActionIntent] = []
    var calls: [String] { lock.lock(); defer { lock.unlock() }; return recordedCalls }
    var calledOnMain: Bool { lock.lock(); defer { lock.unlock() }; return main }
    var observer: (any ClientObserver)? { lock.lock(); defer { lock.unlock() }; return storedObserver }
    var planned: [ActionIntent] { lock.lock(); defer { lock.unlock() }; return plannedIntents }

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
        if case .availability(let intent) = query {
            record("availability")
            if case .sendMessage(_, _, let body, _) = intent, body.isEmpty { throw ClientError.InvalidInput(field: "body", reason: "must not be empty") }
            let blocked: Bool = { if case .stopTeam = intent { return true }; return false }()
            let answer = ActionAvailability(action: blocked ? .stopTeam : .resumeLimited, available: !blocked, reason: blocked ? ClientReason(code: "synthetic", message: "Synthetic Core reason") : nil)
            return ViewData(scope: intent.scope, revision: 1, synchronizedAtMs: nil, freshness: .fresh, payload: .availability(answer))
        }
        record("query")
        return ViewData(scope: CrewTestFixtures.scope, revision: 1, synchronizedAtMs: nil, freshness: .fresh, payload: .team(TeamData(agents: [], relations: [])))
    }
    func plan(intent: ActionIntent) throws -> ActionPlan {
        record("plan")
        lock.lock(); defer { lock.unlock() }
        plannedIntents.append(intent)
        return .dispatch(operationId: 1)
    }
    func confirm(confirmationId: UInt64) throws { record("confirm") }
    func cancelConfirmation(confirmationId: UInt64) throws { record("cancel") }
    func disconnect() throws { record("disconnect") }
    func reconnect() throws { record("reconnect") }
    func selectWorkspace(workspaceId: String) throws { record("select") }
    func setActive(active: Bool) throws { record("active") }
    func enroll(label: String, observer: any SetupObserver) throws -> EnrollmentHandle {
        record("enroll")
        throw GatewayError.Failure(code: "setup_busy", message: "Synthetic client has no gateway.")
    }
    func retireDeviceAccess() throws { record("retire") }
}
