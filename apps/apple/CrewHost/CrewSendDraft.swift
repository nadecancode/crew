import CrewBindings
import Foundation

/// Ephemeral composer state, independent of domain state. A deduplication ID
/// belongs to one logical message body and captured scope. The shared core
/// owns dispatch, retry policy and acknowledgement; this tracks its UI receipt.
///
/// The submitted message lives only in `receipt`, never in the editable body,
/// which may already contain the operator's next message. Everything known
/// about the submitted message is stated on that receipt.
struct CrewSendDraft {
    private(set) var scope: ClientScope
    var body = ""
    private(set) var operationId: UInt64?
    /// A note about the editable text itself, never about the receipt.
    private(set) var explanation: String?
    private(set) var receipt: CrewSendReceipt?
    private var messageId: String?
    private var freshMessageId = UUID().uuidString

    init(scope: ClientScope) { self.scope = scope }

    var pending: Bool { receipt?.isPending == true }
    var held: Bool { receipt?.state == .held }

    /// Retain text and its same-message deduplication ID after reauthorization
    /// of this workspace. Rust retains/replays an identified Send; its final
    /// notice carries the original operation ID with the new verified attempt
    /// scope. UI never replays it.
    mutating func reauthorize(_ current: ClientScope) {
        guard current.endpointId == scope.endpointId, current.workspaceId == scope.workspaceId, current != scope else { return }
        scope = current
        if pending {
            hold()
        } else if !body.isEmpty {
            explanation = "Reconnected. Review the retained text before sending."
        }
    }

    /// The pending receipt itself states that it is held; no separate label
    /// is placed beside the editable text, which may be a different message.
    mutating func hold() {
        guard pending else { return }
        receipt?.state = .held
    }

    func matchesMessage(_ id: String) -> Bool { messageId == id }

    /// The deduplication ID the next explicit Send would use: the retained ID
    /// for an unchanged body, otherwise one fresh ID that stays stable until it
    /// is used. The availability query therefore asks about the exact intent
    /// that Send would dispatch.
    var candidateMessageId: String {
        if let messageId, receipt?.body == body { return messageId }
        return freshMessageId
    }

    /// Client replacement and workspace change end the association while the
    /// Core may already have delivered the message, so no final notice will
    /// arrive. The submitted message stays visible with an unknown outcome and
    /// keeps its ID, so an unchanged explicit retry still deduplicates. A
    /// draft with nothing pending is unaffected.
    mutating func retireOperation() {
        guard pending else { return }
        operationId = nil
        receipt?.state = .retired
    }

    mutating func begin(to recipient: String) -> CrewSendAttempt? {
        guard !pending else { return nil }
        let messageId = candidateMessageId
        if messageId == freshMessageId { freshMessageId = UUID().uuidString }
        self.messageId = messageId
        receipt = CrewSendReceipt(recipient: recipient, body: body, state: .awaiting)
        operationId = nil
        explanation = nil
        return CrewSendAttempt(messageId: messageId, body: body)
    }

    mutating func associate(_ operation: UInt64) { if pending { operationId = operation } }

    mutating func notDispatched(_ reason: String) {
        guard pending else { return }
        operationId = nil
        receipt?.state = .notDispatched(reason)
    }

    /// Correlation is endpoint, workspace and operation ID within one client
    /// instance (the model fences instances). Full scope equality would
    /// reject the Core's replayed final notice, which uses the new attempt.
    mutating func outcome(_ notice: OperationNotice) {
        guard let operationId,
              notice.scope.endpointId == scope.endpointId,
              notice.scope.workspaceId == scope.workspaceId,
              notice.operationId == operationId,
              let submitted = receipt else { return }
        self.operationId = nil
        if notice.kind == .success {
            messageId = nil
            // An acknowledgement may arrive after the operator edited the
            // next message. Only the exact submitted text is cleared; when
            // later edits remain, the success belongs to the receipt.
            if body == submitted.body {
                body = ""
                receipt = nil
                explanation = notice.reason?.message ?? "Message sent."
                return
            }
        }
        // Failed/unknown/unacknowledged same-body retry retains its ID. An
        // explicit different body receives a fresh ID in begin(to:).
        receipt?.state = .outcome(notice)
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
        /// The Core's final notice for this operation.
        case outcome(OperationNotice)
        /// The association ended without a final notice; delivery unknown.
        case retired
        /// Never dispatched; the Core's explanation.
        case notDispatched(String)
    }

    let recipient: String
    let body: String
    var state: State

    var isPending: Bool {
        switch state {
        case .awaiting, .held: return true
        case .outcome, .retired, .notDispatched: return false
        }
    }
}
