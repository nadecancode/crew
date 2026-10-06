import CrewBindings
import Foundation

/// Ephemeral composer state, independent of domain state. A deduplication ID
/// belongs to one logical message body and captured scope. The shared core
/// owns dispatch, retry policy and acknowledgement; this tracks its UI receipt.
struct CrewSendDraft {
    let scope: ClientScope
    var body = ""
    private(set) var pending = false
    private(set) var operationId: UInt64?
    private(set) var lastOutcome: OperationNotice?
    private(set) var explanation: String?
    private var messageId: String?
    private var submittedBody: String?

    mutating func begin() -> CrewSendAttempt? {
        guard !pending else { return nil }
        if messageId == nil || submittedBody != body { messageId = UUID().uuidString }
        guard let messageId else { return nil }
        submittedBody = body
        pending = true
        operationId = nil
        lastOutcome = nil
        explanation = nil
        return CrewSendAttempt(messageId: messageId, body: body)
    }

    mutating func associate(_ operation: UInt64) { operationId = operation }

    mutating func notDispatched(_ explanation: String) {
        pending = false
        operationId = nil
        self.explanation = explanation
    }

    mutating func outcome(_ notice: OperationNotice) {
        guard notice.scope == scope, notice.operationId == operationId else { return }
        lastOutcome = notice
        pending = false
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
