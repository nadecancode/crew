import CrewBindings
import SwiftUI

public struct CrewTaskDetail: View {
    private let scope: ClientScope
    private let task: TaskPresentation
    private let select: (EntityKey) -> Void
    private let copy: (String) -> Void

    public init(scope: ClientScope, task: TaskPresentation, select: @escaping (EntityKey) -> Void, copy: @escaping (String) -> Void) {
        self.scope = scope
        self.task = task
        self.select = select
        self.copy = copy
    }

    public var body: some View {
        List {
            Section {
                Text(verbatim: task.task.title).font(.title2.bold())
                CrewLabeledValue("Task", value: task.task.id, monospaced: true)
                CrewLabeledValue("Status", value: CrewLabels.taskStatus(task.task.status))
                if let assignee = task.task.assignedAgent {
                    Button("Assigned to \(assignee)") { select(EntityKey(scope: scope, entity: .agent(assignee))) }
                }
                if let reviewer = task.task.reviewer {
                    Button("Reviewer: \(reviewer)") { select(EntityKey(scope: scope, entity: .agent(reviewer))) }
                }
                if let progress = task.task.progress { CrewLabeledValue("Progress", value: "\(progress)%") }
                if let timeout = task.task.timeoutSecs { CrewLabeledValue("Timeout", value: "\(timeout.formatted()) seconds") }
                if let note = task.task.note { Text(verbatim: note) }
            }
            Section("Description") { Text(verbatim: task.task.description).textSelection(.enabled) }
            if !task.dependencies.isEmpty {
                Section("Dependencies") {
                    ForEach(task.dependencies, id: \.id) { dependency in
                        Button {
                            select(EntityKey(scope: scope, entity: .task(dependency.id)))
                        } label: {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: dependency.id).font(.body.monospaced())
                                if let status = dependency.status { Text(CrewLabels.taskStatus(status)) }
                                else { Text("Unresolved dependency") }
                            }
                        }
                    }
                }
            }
            if let result = task.task.result { Section("Result") { Text(verbatim: result).textSelection(.enabled) } }
            if !task.task.artifacts.isEmpty {
                Section("Artifacts") {
                    ForEach(Array(task.task.artifacts.enumerated()), id: \.offset) { _, artifact in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: artifact.kind).font(.headline)
                            Text(verbatim: artifact.value).font(.body.monospaced()).textSelection(.enabled)
                            Text(verbatim: artifact.description).textSelection(.enabled)
                            Button("Copy artifact value") { copy(artifact.value) }
                        }
                    }
                }
            }
        }
    }
}

public struct CrewMessageDetail: View {
    private let scope: ClientScope
    private let message: Message
    private let loadedReplies: [Message]
    private let select: (EntityKey) -> Void
    private let copy: (String) -> Void

    public init(scope: ClientScope, message: Message, loadedReplies: [Message], select: @escaping (EntityKey) -> Void, copy: @escaping (String) -> Void) {
        self.scope = scope
        self.message = message
        self.loadedReplies = loadedReplies
        self.select = select
        self.copy = copy
    }

    public var body: some View {
        List {
            Section {
                CrewLabeledValue("Message", value: message.id, monospaced: true)
                CrewLabeledValue("From", value: message.from, monospaced: true)
                CrewLabeledValue("To", value: message.to, monospaced: true)
                CrewLabeledValue("Type", value: CrewLabels.messageType(message.kind))
                CrewLabeledValue("Time", value: CrewLabels.timestamp(message.timestamp))
            }
            Section("Message") {
                Text(verbatim: message.body).textSelection(.enabled)
                Button("Copy message") { copy(message.body) }
            }
            if let parent = message.replyTo {
                Section("Parent") {
                    Button(parent) { select(EntityKey(scope: scope, entity: .message(parent))) }
                }
            }
            if !loadedReplies.isEmpty {
                Section("Loaded replies") {
                    ForEach(loadedReplies, id: \.id) { reply in
                        Button {
                            select(EntityKey(scope: scope, entity: .message(reply.id)))
                        } label: {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: reply.from).font(.headline)
                                Text(verbatim: reply.body).lineLimit(3)
                            }
                        }
                    }
                }
            }
        }
    }
}

public struct CrewEventDetail: View {
    private let event: Event
    private let copy: (String) -> Void

    public init(event: Event, copy: @escaping (String) -> Void) { self.event = event; self.copy = copy }

    public var body: some View {
        List {
            Section {
                CrewLabeledValue("Sequence", value: event.seq.formatted(), monospaced: true)
                CrewLabeledValue("Level", value: CrewLabels.eventLevel(event.level))
                CrewLabeledValue("Source", value: event.source)
                CrewLabeledValue("Type", value: event.eventType)
                CrewLabeledValue("Time", value: CrewLabels.timestamp(event.timestamp))
                if let agent = event.agentId { CrewLabeledValue("Agent", value: agent, monospaced: true) }
                if let task = event.taskId { CrewLabeledValue("Task", value: task, monospaced: true) }
            }
            Section("Event") {
                Text(verbatim: event.message).textSelection(.enabled)
                Button("Copy event") { copy(event.message) }
            }
            if let metadata = event.dataJson {
                Section("Metadata") { Text(verbatim: metadata).font(.body.monospaced()).textSelection(.enabled) }
            }
        }
    }
}

/// The caller supplies the real shared ResourceState and requests refresh via
/// the typed ResourceKey. It never changes a read result into a successful one.
public struct CrewResourceView: View {
    private let resource: ResourceState
    private let refresh: (ResourceKey) -> Void
    private let copy: (String) -> Void

    public init(resource: ResourceState, refresh: @escaping (ResourceKey) -> Void, copy: @escaping (String) -> Void) {
        self.resource = resource
        self.refresh = refresh
        self.copy = copy
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(statusText).font(.callout)
                Spacer()
                Button("Refresh") { refresh(resource.key) }
                    .disabled(resource.status == .loading)
            }
            .padding(16)
            if let error = resource.error { Text(verbatim: error.message).padding(.horizontal, 16) }
            if let value = resource.value {
                content(value)
            } else if resource.status == .loading {
                ProgressView("Loading evidence")
            } else {
                ContentUnavailableView(statusText, systemImage: "doc.text")
            }
        }
    }

    @ViewBuilder private func content(_ value: ResourceValue) -> some View {
        switch value {
        case .diff(let diff): CrewTextEvidence(text: diff.text) { copy(diff.text) }
        case .output(let output): CrewTextEvidence(text: output.text) { copy(output.text) }
        case .transcript(let transcript):
            List {
                if !transcript.available { Text("Transcript unavailable") }
                if transcript.truncated { Text("Showing a bounded transcript read") }
                ForEach(Array(transcript.entries.enumerated()), id: \.offset) { _, entry in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(CrewLabels.transcriptKind(entry.kind)).font(.headline)
                        if let timestamp = entry.at { Text(CrewLabels.timestamp(timestamp)).font(.caption) }
                        Text(verbatim: entry.text).textSelection(.enabled)
                        Button("Copy entry") { copy(entry.text) }
                    }
                }
            }
        case .message(let message):
            if let message { CrewTextEvidence(text: message.body) { copy(message.body) } }
            else { Text("Message unavailable") }
        case .event(let event):
            if let event { CrewEventDetail(event: event, copy: copy) }
            else { Text("Event unavailable") }
        case .taskCard(let card):
            List {
                Text(verbatim: card.task.title).font(.headline)
                CrewLabeledValue("Task", value: card.task.id, monospaced: true)
                Text(verbatim: card.task.description).textSelection(.enabled)
                if let result = card.task.result { Text(verbatim: result).textSelection(.enabled) }
                ForEach(card.dependencies, id: \.id) { dependency in
                    CrewLabeledValue(dependency.id, value: CrewLabels.taskStatus(dependency.status), monospaced: true)
                }
            }
        case .inbox(let inbox):
            List {
                ForEach(inbox.messages, id: \.message.id) { entry in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: "\(entry.message.from) · \(entry.message.to)").font(.headline)
                        Text(verbatim: entry.message.body).textSelection(.enabled)
                        if let delivered = entry.deliveredAt { CrewLabeledValue("Delivered", value: CrewLabels.timestamp(delivered)) }
                        if let acknowledged = entry.ackedAt { CrewLabeledValue("Acknowledged", value: CrewLabels.timestamp(acknowledged)) }
                    }
                }
            }
        case .models(let models):
            List {
                ForEach(Array(models.models.enumerated()), id: \.offset) { _, model in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: model.name).font(.headline)
                        Text(verbatim: model.model).font(.body.monospaced()).textSelection(.enabled)
                    }
                }
            }
        case .history(let messages):
            List {
                ForEach(messages, id: \.id) { message in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: "\(message.from) · \(message.to)").font(.headline)
                        Text(verbatim: message.body).textSelection(.enabled)
                        Text(CrewLabels.timestamp(message.timestamp)).font(.caption)
                    }
                }
            }
        case .events(let events):
            List {
                ForEach(events, id: \.seq) { event in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: event.message).textSelection(.enabled)
                        Text(verbatim: "\(CrewLabels.eventLevel(event.level)) · \(event.source)").font(.callout)
                        Text(CrewLabels.timestamp(event.timestamp)).font(.caption)
                    }
                }
            }
        }
    }

    private var statusText: String {
        switch resource.status {
        case .idle: return "Not loaded"
        case .loading: return "Loading"
        case .ready: return "Loaded"
        case .empty: return "No evidence returned"
        case .unsupported: return "Unavailable for this backend"
        case .failed: return "Evidence read failed"
        case .stale: return "Last-known evidence"
        }
    }
}

public extension CrewLabels {
    static func transcriptKind(_ kind: TranscriptKind) -> String {
        switch kind {
        case .prompt: return "Prompt"
        case .reply: return "Reply"
        case .tool: return "Tool"
        case .note: return "Note"
        }
    }
}
