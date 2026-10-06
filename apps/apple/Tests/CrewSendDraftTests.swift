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
        XCTAssertEqual(draft.explanation, "Message sent.")
        draft.body = "second message"
        let second = try XCTUnwrap(draft.begin(to: recipient))
        XCTAssertNotEqual(first.messageId, second.messageId)
        XCTAssertEqual(second.body, "second message")
    }

    /// Success after later edits belongs to the receipt of the submitted
    /// message, never to the editor, which still holds unsent text.
    func testAcknowledgementDoesNotClearNewEdits() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "submitted text"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.body = "next text typed while waiting"
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "submitted text", state: .awaiting))
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.body, "next text typed while waiting")
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "submitted text", state: .outcome(outcome(1, .success))))
        XCTAssertNil(draft.explanation)
        XCTAssertFalse(draft.pending)
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
            XCTAssertEqual(draft.candidateMessageId, first.messageId)
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

    /// The availability query and Send use the same intent, so the candidate
    /// ID is stable while the text is unchanged and is the one Send uses.
    func testCandidateIdIsStableAndUsedBySend() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "queried text"
        let candidate = draft.candidateMessageId
        XCTAssertEqual(draft.candidateMessageId, candidate)
        let attempt = try XCTUnwrap(draft.begin(to: recipient))
        XCTAssertEqual(attempt.messageId, candidate)
        draft.body = "edited while pending"
        XCTAssertNotEqual(draft.candidateMessageId, candidate)
    }

    func testNotDispatchedStaysOnTheReceipt() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "/stop"
        _ = try XCTUnwrap(draft.begin(to: recipient))
        draft.body = "later text"
        draft.notDispatched("This command requires confirmation. No message has been sent.")
        XCTAssertEqual(draft.receipt?.state, .notDispatched("This command requires confirmation. No message has been sent."))
        XCTAssertNil(draft.explanation)
        XCTAssertFalse(draft.pending)
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
    /// editable draft, and success is stated on A's receipt while B stays.
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
        let success = outcome(1, .success, in: newAttempt)
        draft.outcome(success)
        XCTAssertEqual(draft.body, "message B")
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "message A", state: .outcome(success)))
        XCTAssertNil(draft.explanation)
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

    /// Client replacement and workspace change end the association without a
    /// final notice. The submitted message may already be delivered, so it
    /// stays visible as unknown and keeps its ID for an unchanged retry.
    func testRetirementKeepsSubmittedMessageAsUnknown() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "message A"
        let first = try XCTUnwrap(draft.begin(to: recipient))
        draft.associate(1)
        draft.body = "message B"
        draft.retireOperation()
        XCTAssertEqual(draft.receipt, CrewSendReceipt(recipient: recipient, body: "message A", state: .retired))
        XCTAssertFalse(draft.pending)
        XCTAssertNil(draft.operationId)
        XCTAssertNil(draft.explanation)
        XCTAssertEqual(draft.body, "message B")
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.receipt?.state, .retired)
        draft.body = "message A"
        XCTAssertEqual(try XCTUnwrap(draft.begin(to: recipient)).messageId, first.messageId)
    }

    func testRetirementLeavesDraftsWithNothingPendingUnchanged() throws {
        var never = CrewSendDraft(scope: scope)
        never.body = "never submitted"
        never.retireOperation()
        XCTAssertNil(never.receipt)
        XCTAssertNil(never.explanation)

        var failed = CrewSendDraft(scope: scope)
        failed.body = "rejected"
        _ = try XCTUnwrap(failed.begin(to: recipient))
        failed.associate(1)
        failed.outcome(outcome(1, .rejected))
        failed.retireOperation()
        XCTAssertEqual(failed.receipt?.state, .outcome(outcome(1, .rejected)))
        XCTAssertNil(failed.explanation)
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
