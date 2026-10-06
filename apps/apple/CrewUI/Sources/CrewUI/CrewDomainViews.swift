import CrewBindings
import SwiftUI

/// Native rendering consumes the generated shared projections in their given
/// order. Selection is always an EntityKey containing the full ClientScope.
public struct CrewTeamList: View {
    private let scope: ClientScope
    private let data: TeamData
    private let taskTitle: (String) -> String?
    private let select: (EntityKey) -> Void
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool

    public init(scope: ClientScope, data: TeamData, taskTitle: @escaping (String) -> String?, select: @escaping (EntityKey) -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.scope = scope
        self.data = data
        self.taskTitle = taskTitle
        self.select = select
        self.selected = selected; self.focus = focus; self.active = active
    }

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                ForEach(data.agents, id: \.agent.id) { presentation in
                    Button {
                        select(EntityKey(scope: scope, entity: .agent(presentation.agent.id)))
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(alignment: .firstTextBaseline) {
                                Text(verbatim: presentation.agent.name).font(.headline)
                                Spacer()
                                CrewToneLabel(presentation.statusLabel, tone: presentation.tone)
                            }
                            Text(verbatim: presentation.agent.id).font(.caption.monospaced())
                            Text(verbatim: presentation.agent.role).font(.callout)
                            if let work = presentation.currentWork {
                                Text(verbatim: taskTitle(work.taskId) ?? work.taskId)
                                    .font(.body)
                                HStack {
                                    Text(verbatim: work.taskId).font(.caption.monospaced())
                                    if let elapsed = work.elapsedMs {
                                        Text(Duration.milliseconds(elapsed).formatted(.time(pattern: .minuteSecond)))
                                            .font(.caption.monospacedDigit())
                                    }
                                }
                            }
                            if !presentation.waitingTaskIds.isEmpty {
                                Text("Waiting tasks: \(presentation.waitingTaskIds.count)").font(.callout)
                            }
                            Text(verbatim: presentation.agent.activity).font(.callout).foregroundStyle(.secondary)
                            ForEach(Array(presentation.warnings.enumerated()), id: \.offset) { _, warning in
                                Label(warning.text, systemImage: "exclamationmark.triangle")
                                    .font(.callout)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .agent(presentation.agent.id)), selected: selected, coordinator: focus, active: active))
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("crew.agent.\(presentation.agent.id)")
                }
            }
            .onChange(of: focus?.request?.id) { _, _ in
                if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
            }
        }
        .overlay {
            if data.agents.isEmpty { ContentUnavailableView("No agents", systemImage: "person.3") }
        }
    }
}

public struct CrewTaskList: View {
    private let scope: ClientScope
    private let data: TasksData
    private let select: (EntityKey) -> Void
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool

    public init(scope: ClientScope, data: TasksData, select: @escaping (EntityKey) -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.scope = scope
        self.data = data
        self.select = select
        self.selected = selected; self.focus = focus; self.active = active
    }

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                ForEach(data.tasks, id: \.task.id) { presentation in
                    Button {
                        select(EntityKey(scope: scope, entity: .task(presentation.task.id)))
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: presentation.task.title).font(.headline)
                            HStack {
                                Text(verbatim: presentation.task.id).font(.caption.monospaced())
                                Text(CrewLabels.taskStatus(presentation.task.status)).font(.callout)
                            }
                            if let assignee = presentation.task.assignedAgent {
                                Label(assignee, systemImage: "person")
                            }
                            if let progress = presentation.task.progress {
                                ProgressView(value: Double(progress), total: 100) {
                                    Text("Progress: \(progress)%")
                                }
                            }
                            if let note = presentation.task.note, !note.isEmpty {
                                Text(verbatim: note).font(.callout)
                            }
                            if presentation.inCycle {
                                Label("Dependency cycle", systemImage: "exclamationmark.triangle")
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .task(presentation.task.id)), selected: selected, coordinator: focus, active: active))
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("crew.task.\(presentation.task.id)")
                }
            }
            .onChange(of: focus?.request?.id) { _, _ in
                if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
            }
        }
        .overlay {
            if data.tasks.isEmpty { ContentUnavailableView("No tasks", systemImage: "checklist") }
        }
    }
}

public struct CrewMessageList: View {
    private let scope: ClientScope
    private let page: MessagePage
    private let select: (EntityKey) -> Void
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool
    private let loadOlder: () -> Void

    public init(scope: ClientScope, page: MessagePage, select: @escaping (EntityKey) -> Void, loadOlder: @escaping () -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.scope = scope
        self.page = page
        self.select = select
        self.selected = selected; self.focus = focus; self.active = active
        self.loadOlder = loadOlder
    }

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                ForEach(page.items, id: \.id) { message in
                    Button {
                        select(EntityKey(scope: scope, entity: .message(message.id)))
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: "\(message.from) · \(message.to)").font(.headline)
                            Text(CrewLabels.messageType(message.kind)).font(.callout)
                            Text(verbatim: message.body).lineLimit(3)
                            Text(CrewLabels.timestamp(message.timestamp)).font(.caption)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .message(message.id)), selected: selected, coordinator: focus, active: active))
                    .accessibilityElement(children: .combine)
                }
                if page.hasOlderLocal || page.mayHaveOlderRemote {
                    Button("Load older messages", action: loadOlder)
                }
            }
            .onChange(of: focus?.request?.id) { _, _ in
                if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
            }
        }
        .overlay {
            if page.items.isEmpty { ContentUnavailableView("No messages", systemImage: "bubble.left.and.bubble.right") }
        }
    }
}

public struct CrewEventList: View {
    private let scope: ClientScope
    private let page: EventPage
    private let select: (EntityKey) -> Void
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool
    private let loadOlder: () -> Void

    public init(scope: ClientScope, page: EventPage, select: @escaping (EntityKey) -> Void, loadOlder: @escaping () -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.scope = scope
        self.page = page
        self.select = select
        self.selected = selected; self.focus = focus; self.active = active
        self.loadOlder = loadOlder
    }

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                ForEach(page.items, id: \.seq) { event in
                    Button {
                        // The shared event query's public identity is unsigned;
                        // invalid signed wire values cannot become a navigation ID.
                        if let sequence = UInt64(exactly: event.seq) {
                            select(EntityKey(scope: scope, entity: .event(sequence)))
                        }
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: event.message).font(.body)
                            Text(verbatim: "\(CrewLabels.eventLevel(event.level)) · \(event.source) · \(event.eventType)")
                                .font(.callout)
                            if let agent = event.agentId { Text(verbatim: agent).font(.caption.monospaced()) }
                            Text(CrewLabels.timestamp(event.timestamp)).font(.caption)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 8)
                    }
                    .buttonStyle(.plain)
                    .modifier(CrewEventEntityFocus(sequence: event.seq, scope: scope, selected: selected, coordinator: focus, active: active))
                    .accessibilityElement(children: .combine)
                }
                if page.hasOlderLocal || page.mayHaveOlderRemote {
                    Button("Load older events", action: loadOlder)
                }
            }
            .onChange(of: focus?.request?.id) { _, _ in
                if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
            }
        }
        .overlay {
            if page.items.isEmpty { ContentUnavailableView("No events", systemImage: "list.bullet.rectangle") }
        }
    }
}

private struct CrewEventEntityFocus: ViewModifier {
    let sequence: Int64
    let scope: ClientScope
    let selected: EntityKey?
    let coordinator: CrewFocus?
    let active: Bool
    func body(content: Content) -> some View {
        if let id = UInt64(exactly: sequence) {
            content.modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .event(id)), selected: selected, coordinator: coordinator, active: active))
        } else {
            content.disabled(true)
        }
    }
}

public struct CrewUsageView: View {
    private let scope: ClientScope
    private let usage: UsagePresentation
    private let selectAgent: (String) -> Void
    private let continueLimited: () -> Void
    private let restartLimited: () -> Void
    private let resumeAvailable: Bool
    private let resumeReason: String?
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool

    public init(scope: ClientScope, usage: UsagePresentation, selectAgent: @escaping (String) -> Void, resumeAvailable: Bool, resumeReason: String?, continueLimited: @escaping () -> Void, restartLimited: @escaping () -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.scope = scope; self.selected = selected; self.focus = focus; self.active = active
        self.usage = usage; self.selectAgent = selectAgent
        self.resumeAvailable = resumeAvailable; self.resumeReason = resumeReason
        self.continueLimited = continueLimited; self.restartLimited = restartLimited
    }

    public var body: some View {
        ScrollViewReader { proxy in
            List {
                Section("Reported usage") {
                    Text("Reporting coverage: \(usage.reporters) of \(usage.eligible) agents")
                    if let total = usage.total {
                        CrewTokenChart(usage: total)
                        usageValues(total)
                    } else {
                        Text("No usage reported")
                    }
                    if usage.overflowed { Label("Reported counters exceeded the supported range", systemImage: "exclamationmark.triangle") }
                }
                ForEach(usage.agents, id: \.agentId) { agent in
                    Section(agent.agentId) {
                        Button("Inspect \(agent.agentId)") { selectAgent(agent.agentId) }
                            .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .agent(agent.agentId)), selected: selected, coordinator: focus, active: active))
                        if let context = agent.context {
                            CrewLabeledValue("Context", value: "\(context.percent)%")
                            CrewLabeledValue("Context tokens", value: context.tokens.formatted())
                            CrewLabeledValue("Context window", value: context.window.formatted())
                        } else {
                            Text("Context not reported")
                        }
                        if let reported = agent.usage { usageValues(reported) }
                        else { Text("Usage not reported") }
                        CrewLabeledValue("Compactions", value: agent.compactions.formatted())
                        Text("Context history includes only samples collected during this client session.")
                            .font(.callout).foregroundStyle(.secondary)
                        CrewContextHistory(agent: agent)
                    }
                }
                ForEach(Array(usage.groups.enumerated()), id: \.offset) { _, group in
                    Section("Usage windows · \(String(describing: group.runtime))") {
                        ForEach(Array(group.quotas.enumerated()), id: \.offset) { _, row in
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: row.quota.window).font(.headline)
                                ProgressView(value: Double(row.quota.percentUsed), total: 100) {
                                    Text("\(row.quota.percentUsed.formatted())% used")
                                }
                                if row.stale { Label("Stale quota report", systemImage: "clock.badge.exclamationmark") }
                                CrewLabeledValue("Updated", value: CrewLabels.timestamp(row.quota.updatedAt))
                                if let reset = row.quota.resetsAt { CrewLabeledValue("Resets", value: CrewLabels.timestamp(reset)) }
                            }
                            .padding(.vertical, 8)
                        }
                        if !group.pausedAgentIds.isEmpty {
                            Text("Paused at a usage limit").font(.headline)
                            ForEach(group.pausedAgentIds, id: \.self) { id in Button(id) { selectAgent(id) } }
                            Button("Continue agents paused at limit", action: continueLimited).disabled(!resumeAvailable)
                            Button("Restart agents paused at limit", action: restartLimited).disabled(!resumeAvailable)
                            if let resumeReason { Text(verbatim: resumeReason).font(.callout).foregroundStyle(.secondary) }
                        }
                    }
                }
            }
            .onChange(of: focus?.request?.id) { _, _ in
                if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
            }
        }
    }

    @ViewBuilder private func usageValues(_ value: Usage) -> some View {
        CrewLabeledValue("Input tokens", value: value.inputTokens.formatted())
        CrewLabeledValue("Output tokens", value: value.outputTokens.formatted())
        CrewLabeledValue("Cache read tokens", value: value.cacheReadTokens.formatted())
        CrewLabeledValue("Cache written tokens", value: value.cacheCreationTokens.formatted())
        CrewLabeledValue("Turns", value: value.turns.formatted())
    }
}

public struct CrewToneLabel: View {
    private let label: String
    private let tone: Tone

    public init(_ label: String, tone: Tone) { self.label = label; self.tone = tone }

    public var body: some View {
        Label(label, systemImage: symbol)
            .font(.callout)
            .foregroundStyle(color)
    }

    private var color: Color {
        switch tone {
        case .neutral: return .secondary
        case .info: return CrewPalette.info
        case .success: return CrewPalette.success
        case .warning: return CrewPalette.warning
        case .danger: return CrewPalette.danger
        }
    }

    private var symbol: String {
        switch tone {
        case .neutral: return "circle"
        case .info: return "info.circle"
        case .success: return "checkmark.circle"
        case .warning: return "exclamationmark.triangle"
        case .danger: return "exclamationmark.octagon"
        }
    }
}

/// Localized display names only; ranking, eligibility and state transition
/// policy remain in Rust. Dates are formatted once per supplied view update.
public enum CrewLabels {
    public static func taskStatus(_ status: TaskStatus) -> String {
        switch status {
        case .queued: return "Queued"
        case .ready: return "Ready"
        case .working: return "Working"
        case .waiting: return "Waiting"
        case .blocked: return "Blocked"
        case .review: return "In review"
        case .done: return "Done"
        case .failed: return "Failed"
        }
    }

    public static func messageType(_ type: MessageType) -> String {
        switch type {
        case .task: return "Task"
        case .message: return "Message"
        case .question: return "Question"
        case .answer: return "Answer"
        case .status: return "Status"
        case .reviewRequest: return "Review request"
        case .reviewResult: return "Review result"
        case .artifact: return "Artifact"
        case .error: return "Error"
        }
    }

    public static func eventLevel(_ level: EventLevel) -> String {
        switch level {
        case .debug: return "Debug"
        case .info: return "Info"
        case .warn: return "Warning"
        case .error: return "Error"
        }
    }

    public static func timestamp(_ milliseconds: Int64) -> String {
        Date(timeIntervalSince1970: Double(milliseconds) / 1_000)
            .formatted(date: .abbreviated, time: .standard)
    }
}
