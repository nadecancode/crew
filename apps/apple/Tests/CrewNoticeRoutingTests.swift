import CrewBindings
import XCTest
#if os(macOS)
@testable import CrewMac
#elseif os(iOS)
@testable import CrewIOS
#endif

final class CrewNoticeRoutingTests: XCTestCase {
    func testInputCapacityInvalidationDoesNotEndLiveTerminal() {
        let notice = ClientNotice(scope: scope, revision: 3, changes: changes, endpoint: .authenticated, workspace: .live, reason: nil, outcomes: [])
        XCTAssertFalse(CrewNoticeRouting.terminalUnavailable(notice, hasRenderer: true))
        XCTAssertTrue(CrewNoticeRouting.terminalCapacityOnly(changes, hasRenderer: true))
        XCTAssertFalse(CrewNoticeRouting.endsAttachment(OperationNotice(scope: scope, operationId: 8, attachmentId: 4, kind: .sentUnacknowledged, reason: nil), scope: scope, attachment: 4))
    }

    func testDifferentSuccessfulAttachDoesNotAssociateWithRequestedPanel() {
        let wrong = OperationNotice(scope: scope, operationId: 5, attachmentId: 5, kind: .success, reason: nil)
        XCTAssertFalse(CrewNoticeRouting.belongsToRequestedAttach(wrong, scope: scope, operation: 4))
        let correct = OperationNotice(scope: scope, operationId: 4, attachmentId: 4, kind: .success, reason: nil)
        XCTAssertTrue(CrewNoticeRouting.belongsToRequestedAttach(correct, scope: scope, operation: 4))
    }

    func testOrdinarySuccessCannotInventAnAttachmentId() {
        let ordinary = OperationNotice(scope: scope, operationId: 4, attachmentId: nil, kind: .success, reason: nil)
        XCTAssertFalse(CrewNoticeRouting.belongsToRequestedAttach(ordinary, scope: scope, operation: 4))
    }

    func testClosedNoticeAffectsOnlyItsFullScopedAttachment() {
        let closed = OperationNotice(scope: scope, operationId: 8, attachmentId: 4, kind: .closedLocally, reason: nil)
        XCTAssertTrue(CrewNoticeRouting.endsAttachment(closed, scope: scope, attachment: 4))
        XCTAssertFalse(CrewNoticeRouting.endsAttachment(closed, scope: scope, attachment: 5))
        let otherAttempt = ClientScope(endpointId: scope.endpointId, workspaceId: scope.workspaceId, generation: scope.generation, daemonPid: scope.daemonPid, daemonStartedAtMs: scope.daemonStartedAtMs, attempt: scope.attempt + 1)
        XCTAssertFalse(CrewNoticeRouting.endsAttachment(closed, scope: otherAttempt, attachment: 4))
    }

    func testEstablishedAttachmentNotSentOrCanceledDisablesInput() {
        for kind in [OperationOutcomeKind.notSent, .canceled] {
            let outcome = OperationNotice(scope: scope, operationId: 8, attachmentId: 4, kind: kind, reason: nil)
            XCTAssertTrue(CrewNoticeRouting.endsAttachment(outcome, scope: scope, attachment: 4))
        }
    }

    private let scope = ClientScope(endpointId: "synthetic-endpoint", workspaceId: "synthetic-workspace", generation: 2, daemonPid: 123, daemonStartedAtMs: 1_700_000_000_000, attempt: 4)
    private let changes = ChangeSet(revision: 3, agents: [], tasks: [], removed: [], session: false, questions: false, messages: false, events: false, usage: false, availability: true, pending: true, reset: false)
}
