import CrewBindings
import SwiftUI

/// The generated projection is authoritative for status, current work,
/// relationships and action availability. This view creates only typed intents
/// and native presentation; no eligibility or shared state is recomputed.
public struct CrewAgentDetail: View {
    private let scope: ClientScope
    private let data: AgentDetailData
    private let pending: Bool
    private let select: (EntityKey) -> Void
    private let perform: (ActionIntent) -> Void
    private let evidence: (ResourceKey) -> Void
    private let compose: (EntityKey) -> Void
    private let prompt: (EntityKey) -> Void
    private let attach: (EntityKey) -> Void
    private let copy: (String) -> Void

    public init(scope: ClientScope, data: AgentDetailData, pending: Bool, select: @escaping (EntityKey) -> Void, perform: @escaping (ActionIntent) -> Void, evidence: @escaping (ResourceKey) -> Void, compose: @escaping (EntityKey) -> Void, prompt: @escaping (EntityKey) -> Void, attach: @escaping (EntityKey) -> Void, copy: @escaping (String) -> Void) {
        self.scope = scope; self.data = data; self.pending = pending
        self.select = select; self.perform = perform; self.evidence = evidence
        self.compose = compose; self.prompt = prompt; self.attach = attach; self.copy = copy
    }

    public var body: some View {
        if let presentation = data.agent {
            let agent = presentation.agent
            let key = EntityKey(scope: scope, entity: .agent(agent.id))
            List {
                Section("Agent") {
                    Text(verbatim: agent.name).font(.title2.bold())
                    CrewLabeledValue("ID", value: agent.id, monospaced: true)
                    CrewToneLabel(presentation.statusLabel, tone: presentation.tone)
                }
                if let work = presentation.currentWork {
                    Section("Current work") {
                        Button(work.taskId) { select(EntityKey(scope: scope, entity: .task(work.taskId))) }
                        if let elapsed = work.elapsedMs {
                            CrewLabeledValue("Elapsed", value: Duration.milliseconds(elapsed).formatted(.time(pattern: .minuteSecond)))
                        }
                    }
                }
                if !presentation.waitingTaskIds.isEmpty {
                    Section("Parked work") {
                        ForEach(presentation.waitingTaskIds, id: \.self) { id in
                            Button(id) { select(EntityKey(scope: scope, entity: .task(id))) }
                        }
                    }
                }
                Section("Activity and problems") {
                    Text(verbatim: agent.role).textSelection(.enabled)
                    Text(verbatim: agent.activity).textSelection(.enabled)
                    if let error = agent.error { Text(verbatim: error).foregroundStyle(CrewPalette.danger) }
                    // The Toolbox section below says the same with its Restart.
                    ForEach(Array(presentation.warnings.filter { $0.code != CrewToolboxWords.warningCode }.enumerated()), id: \.offset) { _, warning in
                        Label(warning.text, systemImage: "exclamationmark.triangle")
                    }
                }
                if let parent = presentation.satelliteParentId {
                    Section("Parent") { Button(parent) { select(EntityKey(scope: scope, entity: .agent(parent))) } }
                }
                if !presentation.subagentIds.isEmpty {
                    Section("Subagents") {
                        ForEach(presentation.subagentIds, id: \.self) { id in
                            Button(id) { select(EntityKey(scope: scope, entity: .agent(id))) }
                        }
                    }
                }
                if let toolbox = agent.toolbox {
                    // The plain restart, which resumes the session: the same
                    // availability and reason as the Actions section's.
                    CrewAgentToolbox(toolbox: toolbox) {
                        action(presentation, .restart, title: "Restart") {
                            perform(.agent(scope: scope, id: agent.id, action: .restart))
                        }
                    }
                }
                Section("Actions") {
                    action(presentation, .sendMessage, title: "Message") { compose(key) }
                    action(presentation, .agentInput, title: "Submit terminal prompt") { prompt(key) }
                    action(presentation, .attach, title: "Open terminal") { attach(key) }
                    ForEach(CrewAgentControl.allCases) { control in
                        action(presentation, control.kind, title: control.title) {
                            perform(.agent(scope: scope, id: agent.id, action: control.action))
                        }
                    }
                    action(presentation, .setModel, title: "Model and CLI") {
                        evidence(ResourceKey(entity: key, kind: .models, options: .default))
                    }
                }
                Section("Evidence") {
                    evidenceButton("Transcript", key: key, kind: .transcript, options: .bounded(limit: 100))
                    evidenceButton("Diff", key: key, kind: .diff, options: .diff(against: nil))
                    evidenceButton("Output preview", key: key, kind: .output, options: .output(lines: 200, screen: true, ansi: false))
                    evidenceButton("Inbox", key: key, kind: .inbox, options: .bounded(limit: 100))
                    evidenceButton("History", key: key, kind: .history, options: .bounded(limit: 100))
                    evidenceButton("Events", key: key, kind: .events, options: .bounded(limit: 100))
                }
                taskSection("Assigned tasks", data.assignedTasks)
                taskSection("Review tasks", data.reviewTasks)
                if !data.recentMessages.items.isEmpty {
                    Section("Recent messages") {
                        ForEach(data.recentMessages.items, id: \.id) { message in
                            Button {
                                select(EntityKey(scope: scope, entity: .message(message.id)))
                            } label: {
                                VStack(alignment: .leading, spacing: 8) {
                                    Text(verbatim: "\(message.from) · \(message.to)").font(.headline)
                                    Text(verbatim: message.body).lineLimit(3)
                                }
                            }
                        }
                    }
                }
                if let limit = agent.limit {
                    Section("Usage limit") {
                        Text(verbatim: limit.message).textSelection(.enabled)
                        if let reset = limit.resetsAt { CrewLabeledValue("Resets", value: CrewLabels.timestamp(reset)) }
                        if let retry = limit.retryAt { CrewLabeledValue("Automatic retry", value: CrewLabels.timestamp(retry)) }
                        else { Text("Waiting for an explicit Continue or Restart") }
                        CrewLabeledValue("Attempts", value: limit.attempts.formatted())
                    }
                }
                Section("Process details") {
                    CrewLabeledValue("CLI", value: String(describing: agent.runtime))
                    CrewLabeledValue("Model", value: agent.model ?? "CLI default")
                    CrewLabeledValue("Backend", value: agent.backend)
                    CrewLabeledValue("Work directory", value: agent.workdir, monospaced: true)
                    if let branch = agent.branch { CrewLabeledValue("Branch", value: branch, monospaced: true) }
                    if let pid = agent.pid { CrewLabeledValue("Process", value: pid.formatted(), monospaced: true) }
                    if let started = agent.startedAt { CrewLabeledValue("Started", value: CrewLabels.timestamp(started)) }
                    if let session = agent.sessionId { CrewLabeledValue("Runtime session", value: session, monospaced: true) }
                    CrewLabeledValue("Compactions", value: agent.compactions.formatted())
                    Button("Copy work directory") { copy(agent.workdir) }
                }
            }
        } else {
            ContentUnavailableView("Agent unavailable", systemImage: "person.crop.circle.badge.questionmark")
        }
    }

    @ViewBuilder private func action(_ agent: AgentPresentation, _ kind: ActionKind, title: String, perform: @escaping () -> Void) -> some View {
        let availability = agent.actions.first { $0.action == kind }
        VStack(alignment: .leading, spacing: 4) {
            Button(action: perform) { Text(title).frame(minHeight: 44, alignment: .leading) }
                .disabled(availability?.available != true || pending)
            if let reason = availability?.reason { Text(verbatim: reason.message).font(.callout).foregroundStyle(.secondary) }
            else if availability == nil { Text("Unavailable for this client state").font(.callout).foregroundStyle(.secondary) }
        }
    }

    private func evidenceButton(_ title: String, key: EntityKey, kind: ResourceKind, options: ReadOptions) -> some View {
        Button(title) { evidence(ResourceKey(entity: key, kind: kind, options: options)) }
    }

    @ViewBuilder private func taskSection(_ title: String, _ tasks: [TaskPresentation]) -> some View {
        if !tasks.isEmpty {
            Section(title) {
                ForEach(tasks, id: \.task.id) { task in
                    Button {
                        select(EntityKey(scope: scope, entity: .task(task.task.id)))
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: task.task.title)
                            Text(verbatim: "\(task.task.id) · \(CrewLabels.taskStatus(task.task.status))").font(.callout)
                        }
                    }
                }
            }
        }
    }
}

private enum CrewAgentControl: String, CaseIterable, Identifiable {
    case pause, resume, compact, restart, restartFresh, kill, focus
    var id: String { rawValue }
    var title: String {
        switch self {
        case .pause: return "Pause"
        case .resume: return "Resume"
        case .compact: return "Queue compaction"
        case .restart: return "Restart, resuming session"
        case .restartFresh: return "Restart fresh"
        case .kill: return "Kill agent"
        case .focus: return "Jump to host pane"
        }
    }
    var kind: ActionKind {
        switch self {
        case .pause: return .pause
        case .resume: return .resume
        case .compact: return .compact
        case .restart: return .restart
        case .restartFresh: return .restartFresh
        case .kill: return .kill
        case .focus: return .focus
        }
    }
    var action: AgentAction {
        switch self {
        case .pause: return .pause
        case .resume: return .resume
        case .compact: return .compact
        case .restart: return .restart
        case .restartFresh: return .restartFresh
        case .kill: return .kill
        case .focus: return .focus
        }
    }
}
