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
    private var submittedRecipient: String?

    init(scope: ClientScope) { self.scope = scope }

    /// The exact message the Core holds or last reported on. It is never the
    /// editable body, which may already contain the operator's next message.
    var receipt: CrewSendReceipt? {
        guard let submittedBody, let submittedRecipient else { return nil }
        if pending {
            return CrewSendReceipt(recipient: submittedRecipient, body: submittedBody, state: held ? .held : .awaiting)
        }
        if let lastOutcome, lastOutcome.kind != .success {
            return CrewSendReceipt(recipient: submittedRecipient, body: submittedBody, state: .outcome(lastOutcome))
        }
        return nil
    }

    /// Retain text and its same-message deduplication ID after reauthorization
    /// of this workspace. Rust retains/replays an identified Send; its final
    /// notice carries the original operation ID with the new verified attempt
    /// scope. UI never replays it.
    mutating func reauthorize(_ current: ClientScope) {
        guard current.endpointId == scope.endpointId, current.workspaceId == scope.workspaceId, current != scope else { return }
        scope = current
        if pending {
            hold()
        } else {
            explanation = "Reconnected. Review the retained text before sending."
        }
    }

    /// The pending receipt itself states that it is held; no separate label
    /// is placed beside the editable text, which may be a different message.
    mutating func hold() {
        guard pending else { return }
        held = true
        explanation = nil
    }

    func matchesMessage(_ id: String) -> Bool { messageId == id }

    /// The Core retires a held retry on revocation, workspace removal,
    /// sign-out and client replacement, so no final notice will arrive.
    mutating func retireOperation(_ reason: String) {
        pending = false; held = false; operationId = nil
        lastOutcome = nil
        explanation = reason
    }

    mutating func begin(to recipient: String) -> CrewSendAttempt? {
        guard !pending else { return nil }
        if messageId == nil || submittedBody != body { messageId = UUID().uuidString }
        guard let messageId else { return nil }
        submittedBody = body
        submittedRecipient = recipient
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

    /// Correlation is endpoint, workspace and operation ID within one client
    /// instance (the model fences instances). Full scope equality would
    /// reject the Core's replayed final notice, which uses the new attempt.
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
            submittedRecipient = nil
        }
        // Failed/unknown/unacknowledged same-body retry retains its ID. An
        // explicit different body receives a fresh ID in begin(to:).
    }
}

struct CrewSendAttempt {
    let messageId: String
    let body: String
}

/// Read-only presentation of one submitted message. The wire message ID is
/// deliberately absent; only the body, recipient and receipt state are shown.
struct CrewSendReceipt: Equatable {
    enum State: Equatable {
        case awaiting
        case held
        case outcome(OperationNotice)
    }

    let recipient: String
    let body: String
    let state: State

    var isPending: Bool {
        if case .outcome = state { return false }
        return true
    }
}
