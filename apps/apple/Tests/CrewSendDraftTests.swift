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
        let first = try XCTUnwrap(draft.begin())
        draft.associate(1)
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.body, "")
        draft.body = "second message"
        let second = try XCTUnwrap(draft.begin())
        XCTAssertNotEqual(first.messageId, second.messageId)
        XCTAssertEqual(second.body, "second message")
    }

    func testAcknowledgementDoesNotClearNewEdits() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "submitted text"
        let first = try XCTUnwrap(draft.begin())
        draft.associate(1)
        draft.body = "next text typed while waiting"
        draft.outcome(outcome(1, .success))
        XCTAssertEqual(draft.body, "next text typed while waiting")
        let next = try XCTUnwrap(draft.begin())
        XCTAssertNotEqual(first.messageId, next.messageId)
    }

    func testFailureAndUnknownSameMessageRetryKeepTheIdAndText() throws {
        for kind in [OperationOutcomeKind.rejected, .notSent, .unknown, .sentUnacknowledged] {
            var draft = CrewSendDraft(scope: scope)
            draft.body = "retained message"
            let first = try XCTUnwrap(draft.begin())
            draft.associate(1)
            draft.outcome(outcome(1, kind))
            XCTAssertEqual(draft.body, "retained message")
            let retry = try XCTUnwrap(draft.begin())
            XCTAssertEqual(first.messageId, retry.messageId)
        }
    }

    func testUnknownEditedBodyIsANewLogicalMessage() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "unknown message"
        let unknown = try XCTUnwrap(draft.begin())
        draft.associate(1)
        draft.outcome(outcome(1, .unknown))
        draft.body = "different explicit message"
        XCTAssertNotEqual(unknown.messageId, try XCTUnwrap(draft.begin()).messageId)
    }

    func testWrongOperationAndRetiredScopeCannotClearTheDraft() throws {
        var draft = CrewSendDraft(scope: scope)
        draft.body = "scoped text"
        _ = try XCTUnwrap(draft.begin())
        draft.associate(1)
        draft.outcome(outcome(2, .success))
        let retired = ClientScope(endpointId: scope.endpointId, workspaceId: scope.workspaceId, generation: scope.generation, daemonPid: scope.daemonPid, daemonStartedAtMs: scope.daemonStartedAtMs, attempt: scope.attempt + 1)
        draft.outcome(OperationNotice(scope: retired, operationId: 1, attachmentId: nil, kind: .success, reason: nil))
        XCTAssertTrue(draft.pending)
        XCTAssertEqual(draft.body, "scoped text")
    }

    private let scope = ClientScope(endpointId: "synthetic-endpoint", workspaceId: "synthetic-workspace", generation: 2, daemonPid: 123, daemonStartedAtMs: 1_700_000_000_000, attempt: 4)
    private func outcome(_ operation: UInt64, _ kind: OperationOutcomeKind) -> OperationNotice {
        OperationNotice(scope: scope, operationId: operation, attachmentId: nil, kind: kind, reason: nil)
    }
}
