import CrewBindings
import Foundation

/// Ephemeral composer state, independent of domain state. A deduplication ID
/// belongs to one logical message body and captured scope. The shared core
/// owns dispatch, retry policy and acknowledgement; this tracks its UI receipt.
struct CrewSendDraft {
    private(set) var scope: ClientScope
    var body = ""
    private(set) var pending = false
    private(set) var held = false
    private(set) var operationId: UInt64?
    private(set) var lastOutcome: OperationNotice?
    private(set) var explanation: String?
    private var messageId: String?
    private var submittedBody: String?

    init(scope: ClientScope) { self.scope = scope }

    /// Retain text and its same-message deduplication ID after reauthorization
    /// of this workspace. Rust retains/replays an identified Send; its receipt
    /// keeps the captured proposal scope and operation ID. UI never replays it.
    mutating func reauthorize(_ current: ClientScope) {
        guard current.endpointId == scope.endpointId, current.workspaceId == scope.workspaceId, current != scope else { return }
        scope = current
        if pending {
            hold()
        } else {
            explanation = "Reconnected. Review the retained text before sending."
        }
    }

    mutating func hold() {
        guard pending else { return }
        held = true
        explanation = "Reconnecting; previous send still pending"
    }

    func matchesMessage(_ id: String) -> Bool { messageId == id }

    mutating func retireOperation(_ reason: String) {
        pending = false; held = false; operationId = nil
        explanation = reason
    }

    mutating func begin() -> CrewSendAttempt? {
        guard !pending else { return nil }
        if messageId == nil || submittedBody != body { messageId = UUID().uuidString }
        guard let messageId else { return nil }
        submittedBody = body
        pending = true
        held = false
        operationId = nil
        lastOutcome = nil
        explanation = nil
        return CrewSendAttempt(messageId: messageId, body: body)
    }

    mutating func associate(_ operation: UInt64) { if pending { operationId = operation } }

    mutating func notDispatched(_ explanation: String) {
        pending = false
        held = false
        operationId = nil
        self.explanation = explanation
    }

    mutating func outcome(_ notice: OperationNotice) {
        guard notice.scope.endpointId == scope.endpointId,
              notice.scope.workspaceId == scope.workspaceId,
              notice.operationId == operationId else { return }
        lastOutcome = notice
        pending = false
        held = false
        operationId = nil
        if notice.kind == .success {
            // An acknowledgement may arrive after the operator edited the
            // next message. Only the exact submitted text can be cleared.
            if body == submittedBody { body = "" }
            messageId = nil
            submittedBody = nil
        }
        // Failed/unknown/unacknowledged same-body retry retains its ID. An
        // explicit different body receives a fresh ID in begin().
    }
}

struct CrewSendAttempt {
    let messageId: String
    let body: String
}
