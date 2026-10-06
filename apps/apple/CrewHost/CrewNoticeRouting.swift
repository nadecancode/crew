import CrewBindings

/// Presentation routing only: shared notices remain authoritative. Pending
/// capacity invalidations are not terminal endings or target changes.
enum CrewNoticeRouting {
    static func terminalUnavailable(_ notice: ClientNotice, hasRenderer: Bool) -> Bool {
        hasRenderer && notice.workspace != .live
    }

    static func endsAttachment(_ outcome: OperationNotice, scope: ClientScope, attachment: UInt64) -> Bool {
        guard outcome.scope == scope, outcome.attachmentId == attachment else { return false }
        return outcome.kind == .closedLocally || outcome.kind == .rejected ||
            outcome.kind == .notSent || outcome.kind == .canceled || outcome.kind == .unknown
    }

    static func belongsToRequestedAttach(_ outcome: OperationNotice, scope: ClientScope, operation: UInt64) -> Bool {
        outcome.scope == scope && outcome.operationId == operation && outcome.attachmentId == operation && outcome.kind == .success
    }

    static func terminalCapacityOnly(_ changes: ChangeSet, hasRenderer: Bool) -> Bool {
        hasRenderer && (changes.pending || changes.availability) &&
            !changes.reset && !changes.session && !changes.questions &&
            !changes.messages && !changes.events && !changes.usage &&
            changes.agents.isEmpty && changes.tasks.isEmpty && changes.removed.isEmpty
    }
}
