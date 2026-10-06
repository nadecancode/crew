import CrewBindings
import SwiftUI

/// Native filter editors construct the generated query records. Matching,
/// ordering and history coverage remain shared; Apply is explicit and search
/// is labelled as covering only the loaded window.
struct CrewMessageFilters: View {
    let current: MessageFilter
    let apply: (MessageFilter) -> Void
    @State private var endpointMode = 0
    @State private var first = ""
    @State private var second = ""
    @State private var kind: MessageType?
    @State private var text = ""

    var body: some View {
        DisclosureGroup("Message filters · loaded history only") {
            VStack(alignment: .leading, spacing: 12) {
                Picker("Endpoints", selection: $endpointMode) {
                    Text("All").tag(0); Text("Agent").tag(1); Text("Pair").tag(2)
                }
                if endpointMode != 0 { TextField(endpointMode == 1 ? "Agent ID" : "First agent ID", text: $first).autocorrectionDisabled() }
                if endpointMode == 2 { TextField("Second agent ID", text: $second).autocorrectionDisabled() }
                Picker("Message type", selection: $kind) {
                    Text("All types").tag(MessageType?.none)
                    ForEach(Self.kinds, id: \.self) { kind in Text(CrewLabels.messageType(kind)).tag(Optional(kind)) }
                }
                TextField("Search loaded messages", text: $text)
                Button("Apply message filters") {
                    let endpoints: MessageEndpoints
                    switch endpointMode {
                    case 1: endpoints = .agent(id: first)
                    case 2: endpoints = .pair(first: first, second: second)
                    default: endpoints = .all
                    }
                    apply(MessageFilter(endpoints: endpoints, kind: kind, text: text))
                }
                Text("This does not search messages outside the loaded history window.").font(.caption).foregroundStyle(.secondary)
            }
            .padding(.vertical, 8)
        }
        .padding(12)
        .onChange(of: current, initial: true) { _, value in
            switch value.endpoints {
            case .all: endpointMode = 0; first = ""; second = ""
            case .agent(let id): endpointMode = 1; first = id; second = ""
            case .pair(let a, let b): endpointMode = 2; first = a; second = b
            }
            kind = value.kind; text = value.text
        }
    }

    private static let kinds: [MessageType] = [.task, .message, .question, .answer, .status, .reviewRequest, .reviewResult, .artifact, .error]
}

struct CrewEventFilters: View {
    let current: EventFilter
    let apply: (EventFilter) -> Void
    @State private var agent = ""
    @State private var level: EventLevel?
    @State private var text = ""
    @State private var vcsOnly = false

    var body: some View {
        DisclosureGroup("Event filters · loaded history only") {
            VStack(alignment: .leading, spacing: 12) {
                TextField("Agent ID (optional)", text: $agent).autocorrectionDisabled()
                Picker("Event level", selection: $level) {
                    Text("All levels").tag(EventLevel?.none)
                    ForEach(Self.levels, id: \.self) { level in Text(CrewLabels.eventLevel(level)).tag(Optional(level)) }
                }
                TextField("Search loaded events", text: $text)
                Toggle("VCS events only", isOn: $vcsOnly)
                Button("Apply event filters") { apply(EventFilter(agentId: agent.isEmpty ? nil : agent, level: level, text: text, vcsOnly: vcsOnly)) }
                Text("This does not search events outside the loaded history window.").font(.caption).foregroundStyle(.secondary)
            }
            .padding(.vertical, 8)
        }
        .padding(12)
        .onChange(of: current, initial: true) { _, value in
            agent = value.agentId ?? ""; level = value.level; text = value.text; vcsOnly = value.vcsOnly
        }
    }

    private static let levels: [EventLevel] = [.debug, .info, .warn, .error]
}
