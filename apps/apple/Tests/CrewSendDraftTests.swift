import CrewBindings
import Foundation
import XCTest
#if os(macOS)
@testable import CrewMac
#elseif os(iOS)
@testable import CrewIOS
#endif

final class CrewSendDraftTests: XCTestCase {
    func testTwoSuccessfulMessagesUseDistinctDeduplicationIds() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "first message"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.body, "")
        XCTAssertNil(draft.receipt)
        draft.body = "second message"
        let second = try XCTUnwrap(draft.begin(to: recipient))
        XCTAssertNotEqual(first.messageId, second.messageId)
        XCTAssertEqual(second.body, "second message")
    }

    func testAcknowledgementDoesNotClearNewEdits() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "submitted text"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.body = "next text typed while waiting"
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "submitted text", state: .awaiting))
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.body, "next text typed while waiting")
        XCTAssertNil(draft.receipt)
        let next = try XCTUnwrap(draft.begin(to: recipient))
        XCTAssertNotEqual(first.messageId, next.messageId)
    }

    func testFailureAndUnknownSameMessageRetryKeepTheIdAndText() throws {
        for kind in [OperationOutcomeKind.rejected, .notSent, .unknown, .sentUnacknowledged] {
            var draft = CrewSendDraft(scope: scope)
            draft.body = "retained message"
            let first = try XCTUnwrap(draft.begin(to: recipient))
            draft.associate(1)
            draft.outcome(outcome(1, kind))
            XCTAssertEqual(draft.body, "retained message")
            XCTAssertEqual(draft.receipt?.state, .outcome(outcome(1, kind)))
            let retry = try XCTUnwrap(draft.begin(to: recipient))
            XCTAssertEqual(first.messageId, retry.messageId)
        }
    }

    func testUnknownEditedBodyIsANewLogicalMessage() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "unknown message"
        let unknown = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.outcome(outcome(1, .unknown))
        draft.body = "different explicit message"
        XCTAssertEqual(draft.receipt?.body, "unknown message")
        XCTAssertNotEqual(unknown.messageId, try XCTUnwrap(draft.begin(to: recipient)).messageId)
    }

    func testWrongOperationAndDifferentWorkspaceCannotClearTheDraft() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "scoped text"
        _ = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.outcome(outcome(2, .success))
        let retired = ClientScope(endpointId: scope.endpointId, workspaceId: "different-workspace", generation: scope.generation, daemonPid: scope.daemonPid, daemonStartedAtMs: scope.daemonStartedAtMs, attempt: scope.attempt + 1)
        draft.outcome(OperationNotice(scope: retired, operationId: 1, attachmentId: nil, kind: .success, reason: nil))
        XCTAssertTrue(draft.pending)
        XCTAssertEqual(draft.body, "scoped text")
    }

    /// The Core rebinds its held proposal to the newly verified attempt and
    /// replays it once; the final notice keeps the original operation ID but
    /// carries the new attempt's scope.
    func testHeldSendCompletesWithNewAttemptScopeAndOriginalOperation() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "retained after ordinary loss"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.reauthorize(newAttempt)
        XCTAssertEqual(draft.body, first.body)
        XCTAssertTrue(draft.pending)
        XCTAssertTrue(draft.held)
        XCTAssertEqual(draft.operationId, 1)
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: first.body, state: .held))
        XCTAssertNil(draft.begin(to: recipient))
        draft.outcome(outcome(1, .success, in: newAttempt))
        XCTAssertEqual(draft.body, "")
        XCTAssertFalse(draft.pending)
        XCTAssertFalse(draft.held)
        XCTAssertNil(draft.receipt)
        draft.body = "next message"
        XCTAssertNotEqual(try XCTUnwrap(draft.begin(to: recipient)).messageId, first.messageId)
    }

    /// Send A, edit B, reconnect: the held receipt is still A, B is a separate
    /// editable draft, and success clears only A.
    func testHeldReceiptStaysImmutableBesideLaterEdits() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "message A"
        _ = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.body = "message B"
        draft.reauthorize(newAttempt)
        XCTAssertEqual(draft.receipt?.body, "message A")
        XCTAssertEqual(draft.receipt?.state, .held)
        XCTAssertNil(draft.explanation)
        XCTAssertEqual(draft.body, "message B")
        draft.outcome(outcome(1, .success, in: newAttempt))
        XCTAssertEqual(draft.body, "message B")
        XCTAssertNil(draft.receipt)
        XCTAssertFalse(draft.pending)
    }

    func testHeldUnknownKeepsBodyAndExplicitOutcome() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "held message"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.reauthorize(newAttempt)
        let unknown = outcome(1, .unknown, in: newAttempt)
        draft.outcome(unknown)
        XCTAssertFalse(draft.pending)
        XCTAssertEqual(draft.body, "held message")
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "held message", state: .outcome(unknown)))
        XCTAssertEqual(try XCTUnwrap(draft.begin(to: recipient)).messageId, first.messageId)
    }

    /// Revocation, workspace removal, sign-out and client replacement retire
    /// the Core's held retry, so the receipt is purged and nothing resolves it.
    func testRetirementPurgesHeldReceipt() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "held message"
        _ = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.reauthorize(newAttempt)
        draft.retireOperation("Connection changed; check the host before retrying")
        XCTAssertNil(draft.receipt)
        XCTAssertFalse(draft.pending)
        XCTAssertNil(draft.operationId)
        draft.outcome(outcome(1, .success, in: newAttempt))
        XCTAssertEqual(draft.body, "held message")
        XCTAssertNil(draft.lastOutcome)
    }

    private let recipient = "synthetic-agent"
    private let scope = ClientScope(endpointId: "synthetic-endpoint", workspaceId: "synthetic-workspace", generation: 2, daemonPid: 123, daemonStartedAtMs: 1_700_000_000_000, attempt: 4)
    private var newAttempt: ClientScope {
        ClientScope(endpointId: scope.endpointId, workspaceId: scope.workspaceId, generation: scope.generation, daemonPid: scope.daemonPid, daemonStartedAtMs: scope.daemonStartedAtMs, attempt: scope.attempt + 1)
    }
    private func outcome(_ operation: UInt64, _ kind: OperationOutcomeKind, in noticeScope: ClientScope? = nil) -> OperationNotice {
        OperationNotice(scope: noticeScope ?? scope, operationId: operation, attachmentId: nil, kind: kind, reason: nil)
    }
}
