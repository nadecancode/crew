import CrewBindings
import CrewUI
import Foundation
import Observation
import SwiftUI

/// Per-window presentation over the real Rust client. Cached values are the
/// generated projections themselves. No wire parsing, clock, policy, reducer,
/// optimistic domain mutation, or durable private-content cache lives here.
@MainActor
@Observable
final class CrewSessionModel {
    private let worker = CrewClientWorker()
    private var instance = UUID()
    private var hasClient = false
    private var active = true
    private var readTask: _Concurrency.Task<Void, Never>?
    private var invalidated: Set<ViewQuery> = []
    private var readGeneration: UInt64 = 0
    private var queries: [CrewDestination: ViewQuery] = [:]
    private var viewQueries: [CrewDestination: ViewQuery] = [:]
    private var hostWindowRequested: Set<CrewDestination> = []
    private var olderLoads: [CrewDestination: CrewOlderLoad] = [:]
    private var olderOperations: [UInt64: CrewDestination] = [:]
    private var operationHistory: [OperationNotice] = []
    private var sendOutcomeHistory: [OperationNotice] = []
    private var pendingJobs = 0
    private var executingSend: CrewDraftKey?
    private var actionTask: _Concurrency.Task<Void, Never>?
    private var actionQueue: [CrewQueuedAction] = []
    private var activeAttachRequest: UUID?
    private var actionGeneration: UInt64 = 0
    private var terminalRenderers: [CrewTerminalIdentity: CrewTerminalRenderer] = [:]
    private var availabilityWatchers: [ActionIntent: Int] = [:]
    private var availabilityResults: [ActionIntent: CrewAvailabilityRead.Result] = [:]
    private var availabilityTask: _Concurrency.Task<Void, Never>?
    private var availabilityStale = false
    private var availabilityControls: [String: ActionIntent] = [:]
    private var lastAvailability: [String: CrewControlAvailability] = [:]

    private(set) var localProfiles: [LocalProfileInfo] = []
    private(set) var discovering = false
    private(set) var connectionRequested = false
    private(set) var notice: ClientNotice?
    private(set) var context: ConnectionContext?
    private(set) var workspaces: Workspaces?
    private(set) var views: [CrewDestination: ViewData] = [:]
    private(set) var overview: ViewData?
    private(set) var questions: ViewData?
    private(set) var detail: ViewData?
    private(set) var selected: EntityKey?
    private(set) var resource: ViewData?
    private(set) var selectedResource: ResourceKey?
    private(set) var confirmation: CrewConfirmation?
    var externalLink: CrewLinkPreview?
    private(set) var feedback: String?
    private(set) var error: String?
    private(set) var unknownTerminalChunks: UInt64 = 0
    private(set) var terminal: CrewTerminalRenderer?
    private(set) var terminalStatus: String?
    private(set) var terminalAgent: EntityKey?
    private var attachingOperation: (scope: ClientScope, operation: UInt64, agent: EntityKey)?
    private var expectedAttachmentScope: ClientScope?
    private var drafts: [CrewDraftKey: String] = [:]
    private var sendDrafts: [CrewDraftKey: CrewSendDraft] = [:]

    var destination: CrewDestination = .team {
        didSet { if destination != oldValue { invalidate(query(for: destination)) } }
    }

    var scope: ClientScope? { notice?.scope }
    var pending: Bool { pendingJobs > 0 }
    var terminalPending: Bool { expectedAttachmentScope != nil || attachingOperation != nil }
    var isLive: Bool { active && notice?.workspace == .live && currentView?.freshness == .fresh }
    var currentView: ViewData? { views[destination] }
    var messageFilter: MessageFilter {
        if case .messages(_, _, let filter) = query(for: .messages) { return filter }
        return MessageFilter(endpoints: .all, kind: nil, text: "")
    }
    var eventFilter: EventFilter {
        if case .events(_, _, let filter) = query(for: .events) { return filter }
        return EventFilter(agentId: nil, level: nil, text: "", vcsOnly: false)
    }
    func view(for destination: CrewDestination) -> ViewData? {
        viewQueries[destination] == query(for: destination) ? views[destination] : nil
    }
    func loadingOlder(_ destination: CrewDestination) -> Bool { olderLoads[destination] != nil }
    var presentationStatus: String {
        if !active { return "Suspended; last-known data" }
        return notice.map { CrewFailure.workspace($0.workspace) } ?? "Not connected"
    }

    var session: SessionInfo? {
        guard case .overview(let data) = overview?.payload else { return nil }
        return data.session
    }

    var team: TeamData? {
        guard case .team(let data) = views[.team]?.payload else { return nil }
        return data
    }

    var tasks: TasksData? {
        guard case .tasks(let data) = views[.tasks]?.payload else { return nil }
        return data
    }

    func query(for destination: CrewDestination) -> ViewQuery {
        if let stored = queries[destination] { return stored }
        switch destination {
        case .team: return .team
        case .tasks: return .tasks
        case .messages: return .messages(before: nil, limit: 100, filter: MessageFilter(endpoints: .all, kind: nil, text: ""))
        case .events: return .events(before: nil, limit: 100, filter: EventFilter(agentId: nil, level: nil, text: "", vcsOnly: false))
        case .usage: return .usage
        }
    }

    #if os(macOS)
    func discoverLocal() async {
        guard !discovering else { return }
        discovering = true
        defer { discovering = false }
        retirePresentation()
        let token = instance
        let observer = makeObserver(token)
        do {
            let profiles = try await worker.discoverLocal(observer: observer, instance: token)
            guard instance == token else { return }
            hasClient = true
            try await worker.setActive(active, instance: token)
            localProfiles = profiles
            error = nil
            invalidate(.overview)
        } catch {
            guard instance == token else { return }
            self.error = CrewFailure.message(error)
        }
    }
    #endif

    /// The gateway owner supplies a real generated factory result. No fake
    /// client or unverified endpoint parameters enter connected composition.
    func install(_ client: any CrewClientProtocol) async throws {
        retirePresentation()
        let token = instance
        try await worker.install(client, observer: makeObserver(token), instance: token)
        guard instance == token else { return }
        hasClient = true
        try await worker.setActive(active, instance: token)
        invalidate(.overview)
    }

    func connect(profile: LocalProfileInfo) {
        guard hasClient, !connectionRequested else { return }
        connectionRequested = true
        let token = instance
        _Concurrency.Task {
            do {
                try await worker.connect(profileId: profile.profileId, instance: token)
                guard instance == token else { return }
                connectionRequested = false
                error = nil
                invalidate(.overview)
            } catch {
                guard instance == token else { return }
                connectionRequested = false
                self.error = CrewFailure.message(error)
            }
        }
    }

    func selectWorkspace(_ id: String) {
        cancelConfirmation()
        endTerminal("Workspace changed")
        selected = nil
        detail = nil
        selectedResource = nil
        resource = nil
        readGeneration &+= 1
        let token = instance
        _Concurrency.Task {
            do {
                try await worker.selectWorkspace(id, instance: token)
                guard instance == token else { return }
                invalidate(.overview)
                invalidate(query(for: destination))
            } catch {
                guard instance == token else { return }
                self.error = CrewFailure.message(error)
            }
        }
    }

    func select(_ key: EntityKey) {
        guard key.scope == scope, selected != key else { return }
        cancelConfirmation()
        selected = key
        detail = nil
        selectedResource = nil
        resource = nil
        readGeneration &+= 1
        invalidated = invalidated.filter { query in
            switch query { case .agentDetail, .taskDetail, .evidence: return false; default: return true }
        }
        switch key.entity {
        case .agent(let id): invalidate(.agentDetail(id: id))
        case .task(let id): invalidate(.taskDetail(id: id))
        case .message, .event:
            let kind: ResourceKind
            if case .message = key.entity { kind = .message } else { kind = .event }
            readResource(ResourceKey(entity: key, kind: kind, options: .default))
        case .workspace: break
        }
    }

    func closeDetail() {
        cancelConfirmation()
        selected = nil
        detail = nil
        selectedResource = nil
        resource = nil
        readGeneration &+= 1
    }

    func readResource(_ key: ResourceKey) {
        guard key.entity.scope == scope else { return }
        selectedResource = key
        resource = nil
        readGeneration &+= 1
        invalidate(.evidence(key: key))
        perform(.read(scope: key.entity.scope, resource: ResourceRequest(key: key)))
    }

    func refreshResource(_ key: ResourceKey) {
        guard key == selectedResource else { return }
        perform(.read(scope: key.entity.scope, resource: ResourceRequest(key: key)))
        invalidate(.evidence(key: key))
    }

    func closeResource(_ key: ResourceKey) {
        guard selectedResource == key else { return }
        selectedResource = nil
        resource = nil
        readGeneration &+= 1
    }

    func setMessageFilter(_ filter: MessageFilter) {
        olderLoads[.messages] = nil
        queries[.messages] = .messages(before: nil, limit: 100, filter: filter)
        invalidate(query(for: .messages))
    }

    func setEventFilter(_ filter: EventFilter) {
        olderLoads[.events] = nil
        queries[.events] = .events(before: nil, limit: 100, filter: filter)
        invalidate(query(for: .events))
    }

    func loadOlderMessages() {
        guard !loadingOlder(.messages) else { return }
        guard case .messages(let page) = views[.messages]?.payload,
              case .messages(_, let limit, let filter) = query(for: .messages) else { return }
        let next = ViewQuery.messages(before: page.before, limit: limit, filter: filter)
        if page.hasOlderLocal { queries[.messages] = next; invalidate(next) }
        else if page.mayHaveOlderRemote { requestHostWindow(.messages, next: next, kind: .history, limit: 1_000) }
    }

    func loadOlderEvents() {
        guard !loadingOlder(.events) else { return }
        guard case .events(let page) = views[.events]?.payload,
              case .events(_, let limit, let filter) = query(for: .events) else { return }
        let next = ViewQuery.events(before: page.before, limit: limit, filter: filter)
        if page.hasOlderLocal { queries[.events] = next; invalidate(next) }
        else if page.mayHaveOlderRemote { requestHostWindow(.events, next: next, kind: .events, limit: 5_000) }
    }

    func newestMessages() {
        queries[.messages] = .messages(before: nil, limit: 100, filter: messageFilter)
        invalidate(query(for: .messages))
    }
    func newestEvents() {
        queries[.events] = .events(before: nil, limit: 100, filter: eventFilter)
        invalidate(query(for: .events))
    }

    private func requestHostWindow(_ destination: CrewDestination, next: ViewQuery, kind: ResourceKind, limit: UInt32) {
        guard let scope else { return }
        guard !hostWindowRequested.contains(destination) else {
            feedback = "The bounded host window is already loaded. Older rows outside this window cannot be paged through this protocol."
            return
        }
        // The reviewed native service profile retains 1,000 messages/5,000
        // events. v6 returns newest N; it has no remote before-cursor field.
        let key = ResourceKey(entity: EntityKey(scope: scope, entity: .workspace), kind: kind, options: .bounded(limit: limit))
        olderLoads[destination] = CrewOlderLoad(scope: scope, original: query(for: destination), next: next)
        hostWindowRequested.insert(destination)
        enqueue(.read(scope: scope, resource: ResourceRequest(key: key)), attachmentRequest: nil, olderDestination: destination)
    }

    func draft(_ key: CrewDraftKey) -> Binding<String> {
        Binding(get: { self.drafts[key] ?? "" }, set: { self.drafts[key] = $0 })
    }

    func sendDraft(_ key: CrewDraftKey) -> Binding<String> {
        Binding(get: { self.sendDrafts[key]?.body ?? "" }, set: { text in
            if self.sendDrafts[key] == nil { self.sendDrafts[key] = CrewSendDraft(scope: key.scope) }
            self.sendDrafts[key]?.body = text
        })
    }

    func sendPending(_ key: CrewDraftKey) -> Bool { sendDrafts[key]?.pending == true }

    /// Drain only the current availability read (tests and diagnostics).
    func waitForAvailability() async { await availabilityTask?.value }

    /// Drain only the current UI submission batch. Rust completion remains an
    /// observer outcome; this does not wait for delivery or trigger a replay.
    func waitForSubmissionBatch() async { await actionTask?.value }

    /// A note about the editable text only. Everything about a submitted
    /// message, including its outcome, belongs to its receipt.
    func sendExplanation(_ key: CrewDraftKey) -> String? { sendDrafts[key]?.explanation }

    func sendReceipt(_ key: CrewDraftKey) -> CrewSendReceipt? { sendDrafts[key]?.receipt }

    /// Create the draft record when its composer opens, so the availability
    /// query and an explicit Send use the same deduplication ID.
    func prepareSendDraft(_ key: CrewDraftKey) {
        if sendDrafts[key] == nil { sendDrafts[key] = CrewSendDraft(scope: key.scope) }
    }

    /// The exact intent an explicit Send would dispatch for this draft.
    func sendIntent(_ key: CrewDraftKey, to recipient: String) -> ActionIntent? {
        guard let draft = sendDrafts[key] else { return nil }
        return .sendMessage(scope: key.scope, to: recipient, body: draft.body, messageId: draft.candidateMessageId)
    }

    /// Controls register the exact intent they would submit, under a stable
    /// control name. Each answer is the Core's read-only availability query
    /// for that intent; nothing is planned, reserved or dispatched, and
    /// execution still revalidates.
    func watchAvailability(_ entries: [CrewWatchedIntent]) {
        var unanswered = false
        for entry in entries {
            availabilityWatchers[entry.intent, default: 0] += 1
            availabilityControls[entry.control] = entry.intent
            if let result = availabilityResults[entry.intent] {
                lastAvailability[entry.control] = presentation(result)
            } else {
                unanswered = true
            }
        }
        if unanswered { refreshAvailability() }
    }

    /// A control that moved on to a newer intent keeps its last answer; one
    /// that disappeared forgets it.
    func unwatchAvailability(_ entries: [CrewWatchedIntent]) {
        for entry in entries {
            if availabilityControls[entry.control] == entry.intent {
                availabilityControls[entry.control] = nil
                lastAvailability[entry.control] = nil
            }
            guard let count = availabilityWatchers[entry.intent] else { continue }
            if count > 1 {
                availabilityWatchers[entry.intent] = count - 1
            } else {
                availabilityWatchers[entry.intent] = nil
                availabilityResults[entry.intent] = nil
            }
        }
    }

    /// Eligibility and the Core's canonical reason for one captured intent.
    /// An intent from an earlier connection is never queried against the
    /// current one. While the answer for edited input is outstanding, the
    /// control keeps its previous answer, so typing does not flash; the
    /// planner still revalidates the exact intent on submission.
    func availability(_ intent: ActionIntent?, control: String) -> CrewControlAvailability {
        guard let intent else { return lastAvailability[control] ?? .checking }
        guard hasClient, intent.scope == scope else {
            return CrewControlAvailability(available: false, reason: "Reopen this target from the current workspace connection.")
        }
        if let result = availabilityResults[intent] { return presentation(result) }
        return lastAvailability[control] ?? .checking
    }

    private func presentation(_ result: CrewAvailabilityRead.Result) -> CrewControlAvailability {
        switch result {
        case .answered(let answer): return CrewControlAvailability(available: answer.available, reason: answer.reason?.message)
        case .failed(let error): return CrewControlAvailability(available: false, reason: CrewFailure.message(error))
        }
    }

    /// One in-flight read answers every watched intent in the current scope.
    /// Notices and newly watched input mark it stale; it loops until current.
    private func refreshAvailability() {
        availabilityStale = true
        guard hasClient, availabilityTask == nil else { return }
        let token = instance
        availabilityTask = _Concurrency.Task {
            while instance == token, availabilityStale {
                availabilityStale = false
                let intents = availabilityWatchers.keys.filter { $0.scope == scope }
                guard !intents.isEmpty else { break }
                var results: [ActionIntent: CrewAvailabilityRead.Result] = [:]
                do {
                    let reads = try await worker.availability(intents, instance: token)
                    guard instance == token else { return }
                    for read in reads { results[read.intent] = read.result }
                } catch {
                    guard instance == token else { return }
                    let failure = error as? ClientError ?? .ClosedClient
                    for intent in intents { results[intent] = .failed(failure) }
                }
                for (intent, result) in results where availabilityWatchers[intent] != nil && intent.scope == scope {
                    availabilityResults[intent] = result
                }
                for (control, intent) in availabilityControls {
                    if let result = availabilityResults[intent] { lastAvailability[control] = presentation(result) }
                }
            }
            if instance == token { availabilityTask = nil }
        }
    }

    func send(_ key: CrewDraftKey, to: String) {
        guard key.scope == scope else { return }
        if sendDrafts[key] == nil { sendDrafts[key] = CrewSendDraft(scope: key.scope) }
        sendDrafts[key]?.reauthorize(key.scope)
        guard let attempt = sendDrafts[key]?.begin(to: to) else { return }
        // Same intent as sendIntent(_:to:), which the availability query used.
        enqueue(.sendMessage(scope: key.scope, to: to, body: attempt.body, messageId: attempt.messageId), attachmentRequest: nil, sendDraft: key)
    }

    /// A typed intent is submitted once. Rust preflight supplies eligibility,
    /// exact confirmation targets and real outcomes; mutations are not replayed.
    func perform(_ intent: ActionIntent) {
        enqueue(intent, attachmentRequest: nil)
    }

    private func enqueue(_ intent: ActionIntent, attachmentRequest: UUID?, sendDraft: CrewDraftKey? = nil, olderDestination: CrewDestination? = nil) {
        guard hasClient else {
            if let sendDraft { sendDrafts[sendDraft]?.notDispatched("No client is connected") }
            if let olderDestination { failOlder(olderDestination) }
            return
        }
        // One bounded consumer preserves invocation order, including raw keys.
        // Lifecycle and scope changes discard work rather than replaying it.
        if case .attachInput(_, _, let data) = intent {
            let buffered = actionQueue.reduce(0) { count, intent in
                if case .attachInput(_, _, let bytes) = intent.intent { return count + bytes.count }
                return count
            }
            if data.count > 65_536 - buffered {
                endTerminal("Terminal input backlog exceeded; open a new terminal explicitly")
                discardQueuedActions()
                return
            }
        }
        guard actionQueue.count < 32 else {
            feedback = "Too many pending actions. Wait for the current outcome before trying again."
            if let sendDraft { sendDrafts[sendDraft]?.notDispatched(feedback ?? "Action not dispatched") }
            if let olderDestination { failOlder(olderDestination) }
            if case .attachInput = intent {
                endTerminal("Terminal input backlog exceeded; open a new terminal explicitly")
                discardQueuedActions()
            }
            return
        }
        actionQueue.append(CrewQueuedAction(intent: intent, attachmentRequest: attachmentRequest, sendDraft: sendDraft, olderDestination: olderDestination))
        pendingJobs = actionQueue.count + (actionTask == nil ? 0 : 1)
        guard actionTask == nil else { return }
        let token = instance
        let generation = actionGeneration
        actionTask = _Concurrency.Task {
            while instance == token, actionGeneration == generation, !actionQueue.isEmpty {
                let command = actionQueue.removeFirst()
                let intent = command.intent
                executingSend = command.sendDraft
                pendingJobs = actionQueue.count + 1
                do {
                    let plan = try await worker.plan(intent, instance: token)
                    if instance == token, let key = command.sendDraft,
                       case .sendMessage(_, _, _, let messageId) = intent,
                       sendDrafts[key]?.matchesMessage(messageId) == true {
                        switch plan {
                        case .dispatch(let id):
                            sendDrafts[key]?.associate(id)
                            if let outcome = sendOutcomeHistory.last(where: { $0.operationId == id }) { sendDrafts[key]?.outcome(outcome) }
                        case .unavailable(let reason): sendDrafts[key]?.notDispatched(reason.message)
                        case .confirm: sendDrafts[key]?.notDispatched("This command requires confirmation. No message has been sent.")
                        }
                    }
                    guard instance == token, actionGeneration == generation else { return }
                    executingSend = nil
                    switch plan {
                    case .unavailable(let reason):
                        if let destination = command.olderDestination { failOlder(destination) }
                        feedback = reason.message
                        if let key = command.sendDraft { sendDrafts[key]?.notDispatched(reason.message) }
                        if case .attach = intent { endTerminal(reason.message) }
                    case .confirm(let id, let kind, let scope, let targets):
                        if let key = command.sendDraft {
                            sendDrafts[key]?.notDispatched("This command requires confirmation. No message has been sent.")
                        }
                        guard self.scope == scope else {
                            await worker.cancelConfirmation(id, instance: token)
                            continue
                        }
                        cancelConfirmation()
                        confirmation = CrewConfirmation(id: id, kind: kind, scope: scope, targets: targets)
                    case .dispatch(let id):
                        if let destination = command.olderDestination {
                            olderOperations[id] = destination
                            if let outcome = operationHistory.last(where: { $0.operationId == id && $0.scope == scope }) { finishOlder(outcome) }
                        }
                        feedback = operationHistory.last { $0.operationId == id && $0.scope == self.scope }.map(CrewFailure.outcome) ?? "Submitted; awaiting outcome"
                        if case .attach(let scope, let agent, _, _) = intent {
                            let key = EntityKey(scope: scope, entity: .agent(agent))
                            guard terminalAgent == key, expectedAttachmentScope == scope, activeAttachRequest == command.attachmentRequest else {
                                _ = try? await worker.plan(.detach(scope: scope, attachmentId: id), instance: token)
                                continue
                            }
                            attachingOperation = (scope, id, key)
                            if let outcome = operationHistory.last(where: { $0.scope == scope && $0.operationId == id }) {
                                applyAttachment(outcome)
                            }
                        }
                    }
                } catch {
                    if instance == token, let key = command.sendDraft,
                       case .sendMessage(_, _, _, let messageId) = intent,
                       sendDrafts[key]?.matchesMessage(messageId) == true,
                       sendDrafts[key]?.operationId == nil {
                        sendDrafts[key]?.notDispatched(CrewFailure.message(error))
                    }
                    guard instance == token, actionGeneration == generation else { return }
                    executingSend = nil
                    feedback = CrewFailure.message(error)
                    if let destination = command.olderDestination { failOlder(destination) }
                    if let key = command.sendDraft { sendDrafts[key]?.notDispatched(feedback ?? "Action not dispatched") }
                    if case .attach = intent { endTerminal(feedback ?? "Terminal could not open") }
                }
            }
            if instance == token, actionGeneration == generation { actionTask = nil; pendingJobs = 0 }
        }
    }

    func confirm(_ confirmation: CrewConfirmation) {
        guard self.confirmation == confirmation, scope == confirmation.scope else { return }
        self.confirmation = nil
        let token = instance
        _Concurrency.Task {
            do { try await worker.confirm(confirmation.id, instance: token) }
            catch {
                guard instance == token else { return }
                feedback = CrewFailure.message(error)
            }
        }
    }

    func cancelConfirmation() {
        guard let confirmation else { return }
        self.confirmation = nil
        let token = instance
        _Concurrency.Task { await worker.cancelConfirmation(confirmation.id, instance: token) }
    }

    func attach(_ agent: EntityKey) {
        guard agent.scope == scope, case .agent(let id) = agent.entity else { return }
        closeTerminal()
        terminalAgent = agent
        terminalStatus = "Opening terminal"
        expectedAttachmentScope = agent.scope
        let request = UUID()
        activeAttachRequest = request
        enqueue(.attach(scope: agent.scope, agent: id, rows: 24, cols: 80), attachmentRequest: request)
    }

    func closeTerminal() {
        actionQueue.removeAll { command in
            switch command.intent { case .attachInput, .attachResize: return true; default: return false }
        }
        if let identity = terminal?.identity {
            perform(.detach(scope: identity.scope, attachmentId: identity.attachmentId))
        }
        endTerminal("Terminal closed")
        terminal = nil
        terminalAgent = nil
    }

    func setActive(_ active: Bool) {
        self.active = active
        if !active { cancelConfirmation(); discardQueuedActions(preserveHeldSends: true); endTerminal("Suspended; reconnect before opening a new terminal") }
        guard hasClient else { return }
        let token = instance
        _Concurrency.Task {
            do {
                try await worker.setActive(active, instance: token)
                guard instance == token else { return }
                if active { invalidate(.overview); invalidate(query(for: destination)); refreshAvailability() }
            } catch {
                guard instance == token else { return }
                self.error = CrewFailure.message(error)
            }
        }
    }

    func reconnect() {
        guard hasClient else { return }
        cancelConfirmation()
        discardQueuedActions(preserveHeldSends: true)
        endTerminal("Disconnected; terminal input is not replayed")
        let token = instance
        _Concurrency.Task {
            do { try await worker.reconnect(instance: token) }
            catch { if instance == token { self.error = CrewFailure.message(error) } }
        }
    }

    func disconnect() {
        guard hasClient else { return }
        cancelConfirmation()
        discardQueuedActions(preserveHeldSends: true)
        endTerminal("Disconnected")
        let token = instance
        _Concurrency.Task {
            do { try await worker.disconnect(instance: token) }
            catch { if instance == token { self.error = CrewFailure.message(error) } }
        }
    }

    func close() async {
        let token = instance
        retirePresentation()
        localProfiles = []
        await worker.close(instance: token)
    }

    func questionInfo(_ message: Message) async throws -> QuestionInfo {
        try await worker.questionInfo(message, instance: instance)
    }

    func showError(_ error: Error) { feedback = CrewFailure.message(error) }

    func refresh() { invalidate(.overview); invalidate(query(for: destination)) }

    private func makeObserver(_ token: UUID) -> CrewUIObserver {
        CrewUIObserver(notice: { [weak self] notice in
            guard let self, self.instance == token else { return }
            self.receive(notice)
        }, terminal: { [weak self] chunk in
            guard let self, self.instance == token else { return }
            self.receive(chunk)
        })
    }

    private func receive(_ update: ClientNotice) {
        let oldScope = scope
        notice = update
        if update.scope != oldScope {
            let sameWorkspace = update.scope == nil || oldScope == nil ||
                (update.scope?.endpointId == oldScope?.endpointId && update.scope?.workspaceId == oldScope?.workspaceId)
            clearScopedViews(preserveHeldSends: sameWorkspace)
            availabilityResults.removeAll()
            lastAvailability.removeAll()
            if let current = update.scope {
                for key in sendDrafts.keys {
                    if key.scope.endpointId == current.endpointId && key.scope.workspaceId == current.workspaceId { sendDrafts[key]?.reauthorize(current) }
                    else { sendDrafts[key]?.retireOperation() }
                }
            }
            readGeneration &+= 1
        }
        if update.workspace == .accessRevoked || update.workspace == .removed || update.endpoint == .signedOut {
            drafts = drafts.filter { key, _ in
                guard let oldScope else { return false }
                if update.endpoint == .signedOut { return key.scope.endpointId != oldScope.endpointId }
                return key.scope.endpointId != oldScope.endpointId || key.scope.workspaceId != oldScope.workspaceId
            }
            sendDrafts = sendDrafts.filter { key, _ in
                guard let oldScope else { return false }
                if update.endpoint == .signedOut { return key.scope.endpointId != oldScope.endpointId }
                return key.scope.endpointId != oldScope.endpointId || key.scope.workspaceId != oldScope.workspaceId
            }
            workspaces = nil
            readGeneration &+= 1
            clearScopedViews()
            sendOutcomeHistory.removeAll()
        }
        if !update.changes.removed.isEmpty, let selected, update.changes.removed.contains(selected) { closeDetail() }
        if update.changes.reset {
            cancelConfirmation()
        }
        if CrewNoticeRouting.terminalUnavailable(update, hasRenderer: terminal?.isEnded == false || terminalRenderers.values.contains(where: { !$0.isEnded })) {
            endTerminal("\(CrewFailure.workspace(update.workspace)); terminal input disabled")
        }
        for outcome in update.outcomes {
            sendOutcomeHistory.append(outcome)
            if sendOutcomeHistory.count > 32 { sendOutcomeHistory.removeFirst(sendOutcomeHistory.count - 32) }
            for key in sendDrafts.keys { sendDrafts[key]?.outcome(outcome) }
            operationHistory.append(outcome)
            if operationHistory.count > 32 { operationHistory.removeFirst(operationHistory.count - 32) }
            if outcome.scope == scope {
                finishOlder(outcome)
                feedback = CrewFailure.outcome(outcome)
                // Identified Attach success precedes terminal chunks. Create
                // its exact native renderer before data, even when the actor's
                // plan continuation has not associated the panel yet.
                if outcome.kind == .success, let id = outcome.attachmentId {
                    if let attachingOperation,
                       !CrewNoticeRouting.belongsToRequestedAttach(outcome, scope: attachingOperation.scope, operation: attachingOperation.operation) {
                        // Another identified attachment never becomes the
                        // requested panel, even in the same client/scope.
                        perform(.detach(scope: outcome.scope, attachmentId: id))
                        continue
                    }
                    let identity = CrewTerminalIdentity(scope: outcome.scope, attachmentId: id)
                    if terminalRenderers[identity] == nil && expectedAttachmentScope == outcome.scope {
                        if terminalRenderers.count < 2 { terminalRenderers[identity] = makeRenderer(identity) }
                        else { endTerminal("Too many terminal attachments; open a new terminal explicitly") }
                    }
                }
                applyAttachment(outcome)
                if let identity = terminal?.identity,
                   CrewNoticeRouting.endsAttachment(outcome, scope: identity.scope, attachment: identity.attachmentId) {
                    endTerminal(CrewFailure.outcome(outcome))
                }
            }
        }
        let changes = update.changes
        let terminalCapacityOnly = CrewNoticeRouting.terminalCapacityOnly(changes, hasRenderer: terminal != nil)
        if changes.reset || changes.session || (changes.availability && !terminalCapacityOnly) { invalidate(.overview) }
        if changes.reset || changes.questions { invalidate(.questions) }
        if changes.reset || (!terminalCapacityOnly && (changes.availability || changes.pending)) || affects(destination, changes) { invalidate(query(for: destination)) }
        if let selected {
            switch selected.entity {
            case .agent(let id):
                if changes.reset || changes.agents.contains(id) || !changes.tasks.isEmpty || changes.messages || (!terminalCapacityOnly && (changes.availability || changes.pending)) {
                    invalidate(.agentDetail(id: id))
                }
            case .task(let id): if changes.reset || changes.tasks.contains(id) { invalidate(.taskDetail(id: id)) }
            default: break
            }
        }
        if let selectedResource, (changes.pending && !terminalCapacityOnly) || changes.reset { invalidate(.evidence(key: selectedResource)) }
        // Any shared change may alter eligibility; answers for watched controls
        // are re-read together, never inferred from the change set.
        refreshAvailability()
    }

    private func receive(_ chunk: TerminalChunk) {
        guard chunk.scope == scope, active else { unknownTerminalChunks &+= 1; return }
        let identity = CrewTerminalIdentity(scope: chunk.scope, attachmentId: chunk.attachmentId)
        guard let renderer = terminalRenderers[identity] else { unknownTerminalChunks &+= 1; return }
        renderer.receive(chunk)
        if let ended = chunk.ended, terminal?.identity == identity {
            terminalStatus = CrewFailure.terminal(ended)
        }
    }

    private func applyAttachment(_ outcome: OperationNotice) {
        guard let attachingOperation, outcome.scope == attachingOperation.scope, outcome.operationId == attachingOperation.operation else { return }
        switch outcome.kind {
        case .success:
            if let id = outcome.attachmentId {
                let identity = CrewTerminalIdentity(scope: outcome.scope, attachmentId: id)
                for other in Array(terminalRenderers.keys) where other != identity {
                    terminalRenderers.removeValue(forKey: other)?.end()
                    perform(.detach(scope: other.scope, attachmentId: other.attachmentId))
                }
                terminal = terminalRenderers[identity]
                self.attachingOperation = nil
                expectedAttachmentScope = nil
                terminalStatus = terminal?.endReason.map(CrewFailure.terminal) ?? "Terminal open"
                terminal?.setInputAvailable(active && notice?.workspace == .live)
            }
        case .rejected, .notSent, .canceled, .unknown, .closedLocally:
            endTerminal(CrewFailure.outcome(outcome))
        case .sentUnacknowledged: break
        }
    }

    private func makeRenderer(_ identity: CrewTerminalIdentity) -> CrewTerminalRenderer {
        let renderer = CrewTerminalRenderer(identity: identity, input: { [weak self] identity, data in
            guard let self, self.active, self.terminal?.identity == identity, self.scope == identity.scope else { return }
            self.perform(.attachInput(scope: identity.scope, attachmentId: identity.attachmentId, data: data))
        }, resize: { [weak self] identity, rows, cols in
            guard let self, self.active, self.terminal?.identity == identity, self.scope == identity.scope,
                  let rows = UInt16(exactly: rows), let cols = UInt16(exactly: cols), rows > 0, cols > 0 else { return }
            self.perform(.attachResize(scope: identity.scope, attachmentId: identity.attachmentId, rows: rows, cols: cols))
        }, openLink: { [weak self] candidate in
            if let url = CrewExternalLinks.preview(candidate) { self?.externalLink = CrewLinkPreview(url: url) }
            else { self?.feedback = "This address cannot be opened. Copy and review it as text." }
        })
        // Core acceptance of this exact scoped attach is required. Until its
        // real outcome arrives, early bytes may render but input stays disabled.
        renderer.setInputAvailable(false)
        return renderer
    }

    private func endTerminal(_ message: String) {
        for renderer in terminalRenderers.values { renderer.end() }
        terminalRenderers.removeAll()
        terminalStatus = message
        activeAttachRequest = nil
        attachingOperation = nil
        expectedAttachmentScope = nil
    }

    private func affects(_ destination: CrewDestination, _ changes: ChangeSet) -> Bool {
        switch destination {
        case .team: return !changes.agents.isEmpty || !changes.tasks.isEmpty || !changes.removed.isEmpty
        case .tasks: return !changes.tasks.isEmpty || !changes.removed.isEmpty
        case .messages: return changes.messages
        case .events: return changes.events
        case .usage: return changes.usage || !changes.agents.isEmpty
        }
    }

    private func invalidate(_ query: ViewQuery) {
        // Retain only the current request for each presentation slot. Changed
        // filter text or entity IDs cannot grow a backlog of obsolete reads.
        invalidated = invalidated.filter { old in
            switch (old, query) {
            case (.messages, .messages), (.events, .events), (.agentDetail, .agentDetail),
                 (.taskDetail, .taskDetail), (.evidence, .evidence): return false
            default: return true
            }
        }
        invalidated.insert(query)
        guard hasClient, readTask == nil else { return }
        let token = instance
        readTask = _Concurrency.Task {
            while instance == token, !invalidated.isEmpty {
                let requests = Array(invalidated)
                invalidated.removeAll()
                let generation = readGeneration
                do {
                    let result = try await worker.read(requests, instance: token)
                    guard instance == token else { return }
                    if generation == readGeneration {
                        if result.context?.scope == scope { context = result.context }
                        workspaces = result.workspaces
                    }
                    for item in result.views where item.data.scope == scope {
                        apply(item, generation: generation)
                    }
                } catch {
                    guard instance == token else { return }
                    self.error = CrewFailure.message(error)
                }
            }
            if instance == token { readTask = nil }
        }
    }

    private func apply(_ item: CrewQueriedView, generation: UInt64) {
        switch item.query {
        case .overview: overview = item.data
        case .questions: questions = item.data
        case .agentDetail, .taskDetail:
            guard generation == readGeneration else { return }
            detail = item.data
        case .evidence(let key):
            guard generation == readGeneration, selectedResource == key else { return }
            resource = item.data
        default:
            for destination in CrewDestination.allCases where item.query == query(for: destination) {
                views[destination] = item.data
                viewQueries[destination] = item.query
            }
        }
    }

    private func discardQueuedActions(preserveHeldSends: Bool = true) {
        for destination in olderLoads.keys { hostWindowRequested.remove(destination) }
        olderLoads.removeAll()
        olderOperations.removeAll()
        if let key = executingSend {
            if preserveHeldSends { sendDrafts[key]?.hold() }
            else { sendDrafts[key]?.retireOperation() }
            executingSend = nil
        }
        for command in actionQueue {
            if let key = command.sendDraft { sendDrafts[key]?.notDispatched("Action not sent after the connection changed") }
        }
        actionQueue.removeAll()
        actionTask?.cancel()
        actionTask = nil
        actionGeneration &+= 1
        pendingJobs = 0
        for key in sendDrafts.keys {
            if preserveHeldSends { sendDrafts[key]?.hold() }
            else { sendDrafts[key]?.retireOperation() }
        }
    }

    private func clearScopedViews(preserveHeldSends: Bool = false) {
        invalidated.removeAll()
        discardQueuedActions(preserveHeldSends: preserveHeldSends)
        cancelConfirmation()
        endTerminal("Workspace connection changed")
        terminal = nil
        terminalAgent = nil
        context = nil
        externalLink = nil
        views = [:]
        viewQueries = [:]
        olderLoads = [:]
        olderOperations = [:]
        hostWindowRequested = []
        overview = nil
        questions = nil
        selected = nil
        detail = nil
        selectedResource = nil
        resource = nil
        queries = [:]
        operationHistory = []
        if !preserveHeldSends { sendOutcomeHistory = [] }
    }

    private func finishOlder(_ outcome: OperationNotice) {
        guard let destination = olderOperations.removeValue(forKey: outcome.operationId),
              let load = olderLoads.removeValue(forKey: destination), load.scope == outcome.scope else { return }
        if outcome.kind == .success {
            if query(for: destination) == load.original { queries[destination] = load.next; invalidate(load.next) }
        } else {
            hostWindowRequested.remove(destination)
            feedback = CrewFailure.outcome(outcome)
        }
    }

    private func failOlder(_ destination: CrewDestination) {
        olderLoads[destination] = nil
        hostWindowRequested.remove(destination)
    }

    private func retirePresentation() {
        instance = UUID()
        hasClient = false
        readTask?.cancel()
        readTask = nil
        availabilityTask?.cancel()
        availabilityTask = nil
        availabilityStale = false
        availabilityResults.removeAll()
        lastAvailability.removeAll()
        invalidated = []
        notice = nil
        workspaces = nil
        pendingJobs = 0
        connectionRequested = false
        clearScopedViews()
    }
}

private struct CrewQueuedAction {
    let intent: ActionIntent
    let attachmentRequest: UUID?
    let sendDraft: CrewDraftKey?
    let olderDestination: CrewDestination?
}

private struct CrewOlderLoad {
    let scope: ClientScope
    let original: ViewQuery
    let next: ViewQuery
}

struct CrewDraftKey: Hashable {
    let scope: ClientScope
    let purpose: String
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.scope.endpointId == rhs.scope.endpointId &&
            lhs.scope.workspaceId == rhs.scope.workspaceId && lhs.purpose == rhs.purpose
    }
    func hash(into hasher: inout Hasher) {
        hasher.combine(scope.endpointId)
        hasher.combine(scope.workspaceId)
        hasher.combine(purpose)
    }
}

struct CrewConfirmation: Identifiable, Equatable {
    let id: UInt64
    let kind: ConfirmationKind
    let scope: ClientScope
    let targets: [String]
}

/// Safe user-facing formatting of typed outcomes, not diagnostic reflection.
/// No arbitrary transport error string, key reference or wire frame is shown.
enum CrewFailure {
    static func message(_ error: Error) -> String {
        guard let error = error as? ClientError else { return "The client operation could not complete. Reconnect or try again explicitly." }
        switch error {
        case .ClosedClient: return "Connection is closed. Open Connections to reconnect."
        case .CursorExpired: return "This history page expired. Return to the newest page."
        case .StaleConfirmation: return "The target or its availability changed. Review the action again."
        case .InvalidInput(let field, let reason): return "\(field): \(reason)"
        case .DecodeFailure: return "The host response could not be read. Check the app and host versions."
        case .ProtocolMismatch(let expected, let actual, _): return "Protocol mismatch: app \(expected), host \(actual.map(String.init) ?? "unknown")."
        case .TooLarge: return "The response exceeded the supported size. Request a smaller read."
        case .InternalFailure: return "The client operation could not complete. Reconnect or try again explicitly."
        }
    }

    static func receipt(_ receipt: CrewSendReceipt) -> String {
        switch receipt.state {
        case .awaiting: return "Submitted; awaiting the host's outcome."
        case .held: return "Still pending after reconnect. This app will not send it again; the host's outcome resolves it."
        case .outcome(let notice): return notice.kind == .success ? (notice.reason?.message ?? "Sent.") : outcome(notice)
        case .retired: return "Outcome unknown after the connection changed. This app will not send it again; check the host before retrying."
        case .notDispatched(let reason): return reason
        }
    }

    static func outcome(_ notice: OperationNotice) -> String {
        if let reason = notice.reason { return reason.message }
        switch notice.kind {
        case .success: return "Action completed"
        case .rejected: return "Action rejected"
        case .notSent: return "Action was not sent"
        case .canceled: return "Action canceled"
        case .unknown: return "Outcome unknown. Check the host before retrying."
        case .closedLocally: return "Closed in this app"
        case .sentUnacknowledged: return "Sent; acknowledgement unavailable. Check before retrying."
        }
    }

    static func workspace(_ status: WorkspaceStatus) -> String {
        switch status {
        case .noneSelected: return "Choose a workspace"
        case .connecting: return "Synchronizing team"
        case .live: return "Live"
        case .reconnecting: return "Reconnecting; last-known data"
        case .stopped: return "Team is not running"
        case .unreachable: return "Team cannot be reached"
        case .incompatible: return "The app and host use incompatible versions"
        case .removed, .accessRevoked: return "This workspace is no longer available to this device"
        case .neverJoined: return "Workspace is not joined"
        }
    }

    static func terminal(_ end: TerminalEnd) -> String {
        switch end {
        case .normal: return "Terminal ended"
        case .detach: return "Terminal detached"
        case .disconnect: return "Terminal disconnected"
        case .revocation: return "Terminal access revoked"
        case .overflow: return "Terminal output overflowed; open a new terminal explicitly"
        case .unsupported: return "Interactive terminal is unavailable for this backend"
        case .failed(let reason): return reason.message
        }
    }
}

/// One visible control and the exact intent it would submit now.
struct CrewWatchedIntent: Equatable {
    let control: String
    let intent: ActionIntent
}

/// Registers the exact intents visible controls would submit. New intents
/// are watched before old ones are released, so a control keeps its last
/// answer while the input around it changes.
struct CrewAvailabilityWatch: ViewModifier {
    let model: CrewSessionModel
    let intents: [CrewWatchedIntent]

    func body(content: Content) -> some View {
        content
            .onAppear { model.watchAvailability(intents) }
            .onDisappear { model.unwatchAvailability(intents) }
            .onChange(of: intents) { old, new in
                // Only changed entries move; an unchanged control keeps its
                // registration and answer.
                model.watchAvailability(new.filter { !old.contains($0) })
                model.unwatchAvailability(old.filter { !new.contains($0) })
            }
    }
}

extension View {
    func crewAvailability(_ model: CrewSessionModel, _ intents: [CrewWatchedIntent]) -> some View {
        modifier(CrewAvailabilityWatch(model: model, intents: intents))
    }
}
