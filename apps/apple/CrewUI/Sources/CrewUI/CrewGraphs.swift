import CrewBindings
import SwiftUI

/// Native geometry only. Shared projections supply node order, current work,
/// relationships, dependency layers and cycles. Zoom and pan are local view
/// state; an incoming push never refits the operator's graph.
public struct CrewTeamGraph: View {
    private let scope: ClientScope
    private let team: TeamData
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool
    private let select: (EntityKey) -> Void
    private let pairMessages: (String, String) -> Void

    public init(scope: ClientScope, team: TeamData, select: @escaping (EntityKey) -> Void, pairMessages: @escaping (String, String) -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.selected = selected; self.focus = focus; self.active = active
        self.scope = scope; self.team = team; self.select = select; self.pairMessages = pairMessages
    }

    public var body: some View {
        CrewGraphViewport(size: size, focus: focus, active: active) {
            ZStack(alignment: .topLeading) {
                Canvas { context, _ in
                    for relation in team.relations {
                        if let parent = index(relation.parentId), let child = index(relation.childId) {
                            CrewGraphGeometry.edge(from: point(parent), to: point(child), context: &context)
                        }
                    }
                }
                .accessibilityHidden(true)
                ForEach(Array(team.agents.enumerated()), id: \.element.agent.id) { offset, presentation in
                    Button { select(EntityKey(scope: scope, entity: .agent(presentation.agent.id))) } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: presentation.agent.name).font(.headline)
                            Text(verbatim: presentation.agent.id).font(.caption.monospaced())
                            CrewToneLabel(presentation.statusLabel, tone: presentation.tone)
                            if let work = presentation.currentWork { Text(verbatim: work.taskId).font(.callout.monospaced()) }
                            Text(verbatim: presentation.agent.role).font(.callout).lineLimit(2)
                        }
                        .frame(width: 220, height: 144, alignment: .topLeading)
                        .padding(12)
                        .background(CrewPalette.raised, in: RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).stroke(.secondary.opacity(0.5)))
                    }
                    .buttonStyle(.plain)
                    .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .agent(presentation.agent.id)), selected: selected, coordinator: focus, active: active))
                    .position(point(offset))
                    .accessibilityLabel("\(presentation.agent.id), \(presentation.statusLabel), current task \(presentation.currentWork?.taskId ?? "none")")
                    .accessibilityIdentifier("crew.graph.agent.\(presentation.agent.id)")
                }
            }
            .frame(width: size.width, height: size.height)
        } relationships: {
            if !team.relations.isEmpty {
                DisclosureGroup("Parent and subagent relationships") {
                    ForEach(Array(team.relations.enumerated()), id: \.offset) { _, relation in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(verbatim: "\(relation.parentId) is parent of \(relation.childId)")
                            Button("Messages between these agents") { pairMessages(relation.parentId, relation.childId) }
                        }
                        .padding(.vertical, 8)
                    }
                }
            }
        }
    }

    private var size: CGSize { CGSize(width: 1_180, height: max(240, CGFloat((team.agents.count + 3) / 4) * 210 + 30)) }
    private func index(_ id: String) -> Int? { team.agents.firstIndex { $0.agent.id == id } }
    private func point(_ index: Int) -> CGPoint { CGPoint(x: CGFloat(index % 4) * 290 + 145, y: CGFloat(index / 4) * 210 + 105) }
}

public struct CrewTaskGraph: View {
    private let scope: ClientScope
    private let tasks: TasksData
    private let selected: EntityKey?
    private let focus: CrewFocus?
    private let active: Bool
    private let select: (EntityKey) -> Void

    public init(scope: ClientScope, tasks: TasksData, select: @escaping (EntityKey) -> Void, selected: EntityKey? = nil, focus: CrewFocus? = nil, active: Bool = true) {
        self.selected = selected; self.focus = focus; self.active = active
        self.scope = scope; self.tasks = tasks; self.select = select
    }

    public var body: some View {
        CrewGraphViewport(size: size, focus: focus, active: active) {
            ZStack(alignment: .topLeading) {
                Canvas { context, _ in
                    for task in tasks.tasks {
                        guard let target = point(task.task.id) else { continue }
                        for dependency in task.dependencies {
                            if let origin = point(dependency.id) { CrewGraphGeometry.edge(from: origin, to: target, context: &context) }
                        }
                    }
                }
                .accessibilityHidden(true)
                ForEach(tasks.tasks, id: \.task.id) { presentation in
                    if let point = point(presentation.task.id) {
                        Button { select(EntityKey(scope: scope, entity: .task(presentation.task.id))) } label: {
                            VStack(alignment: .leading, spacing: 8) {
                                Text(verbatim: presentation.task.title).font(.headline).lineLimit(2)
                                Text(verbatim: presentation.task.id).font(.caption.monospaced())
                                Text(CrewLabels.taskStatus(presentation.task.status)).font(.callout)
                                if let assignee = presentation.task.assignedAgent { Text(verbatim: assignee).font(.caption) }
                                if presentation.inCycle { Label("Dependency cycle", systemImage: "exclamationmark.triangle").font(.callout) }
                            }
                            .frame(width: 220, height: 144, alignment: .topLeading)
                            .padding(12)
                            .background(CrewPalette.raised, in: RoundedRectangle(cornerRadius: 12))
                            .overlay(RoundedRectangle(cornerRadius: 12).stroke(.secondary.opacity(0.5)))
                        }
                        .buttonStyle(.plain)
                        .modifier(CrewEntityFocus(key: EntityKey(scope: scope, entity: .task(presentation.task.id)), selected: selected, coordinator: focus, active: active))
                        .position(point)
                        .accessibilityIdentifier("crew.graph.task.\(presentation.task.id)")
                    }
                }
            }
            .frame(width: size.width, height: size.height)
        } relationships: {
            DisclosureGroup("Dependency relationships") {
                ForEach(tasks.tasks, id: \.task.id) { task in
                    ForEach(task.dependencies, id: \.id) { dependency in
                        Text(verbatim: "\(task.task.id) depends on \(dependency.id): \(dependency.status.map(CrewLabels.taskStatus) ?? "unresolved dependency")")
                            .padding(.vertical, 4)
                    }
                }
            }
        }
    }

    private var size: CGSize {
        CGSize(width: max(300, CGFloat(tasks.layers.count) * 290 + 30), height: max(240, CGFloat(tasks.layers.map { $0.taskIds.count }.max() ?? 0) * 210 + 30))
    }
    private func point(_ id: String) -> CGPoint? {
        for (column, layer) in tasks.layers.enumerated() {
            if let row = layer.taskIds.firstIndex(of: id) { return CGPoint(x: CGFloat(column) * 290 + 145, y: CGFloat(row) * 210 + 105) }
        }
        return nil
    }
}

private struct CrewGraphViewport<Nodes: View, Relationships: View>: View {
    let size: CGSize
    let focus: CrewFocus?
    let active: Bool
    private let nodes: () -> Nodes
    private let relationships: () -> Relationships
    @State private var zoom: CGFloat = 1

    init(size: CGSize, focus: CrewFocus?, active: Bool, @ViewBuilder nodes: @escaping () -> Nodes, @ViewBuilder relationships: @escaping () -> Relationships) {
        self.focus = focus; self.active = active
        self.size = size; self.nodes = nodes; self.relationships = relationships
    }

    var body: some View {
        VStack(spacing: 0) {
            GeometryReader { viewport in
                VStack(spacing: 0) {
                    HStack {
                        Button("Fit") { zoom = max(0.3, min(1, min(viewport.size.width / size.width, (viewport.size.height - 52) / size.height))) }
                        Button("Zoom out", systemImage: "minus.magnifyingglass") { zoom = max(0.3, zoom / 1.25) }
                        Text("\(Int(zoom * 100))%").monospacedDigit().accessibilityLabel("Graph zoom \(Int(zoom * 100)) percent")
                        Button("Zoom in", systemImage: "plus.magnifyingglass") { zoom = min(2, zoom * 1.25) }
                        Spacer()
                    }
                    .frame(minHeight: 44)
                    .padding(.horizontal, 12)
                    ScrollViewReader { proxy in
                        ScrollView([.horizontal, .vertical]) {
                            nodes()
                                .scaleEffect(zoom, anchor: .topLeading)
                                .frame(width: size.width * zoom, height: size.height * zoom, alignment: .topLeading)
                        }
                        .onChange(of: focus?.request?.id) { _, _ in
                            if active, let key = focus?.request?.target { proxy.scrollTo(key, anchor: .center) }
                        }
                    }
                    .accessibilityLabel("Native graph. Use the List view for complete text and keyboard navigation.")
                }
            }
            ScrollView { relationships().padding(12).frame(maxWidth: .infinity, alignment: .leading) }
                .frame(maxHeight: 160)
        }
        .background(CrewPalette.canvas)
    }
}

private enum CrewGraphGeometry {
    static func edge(from origin: CGPoint, to target: CGPoint, context: inout GraphicsContext) {
        var path = Path()
        path.move(to: origin)
        path.addLine(to: target)
        context.stroke(path, with: .color(.secondary), lineWidth: 1.5)
        let angle = atan2(target.y - origin.y, target.x - origin.x)
        let marker = CGPoint(x: (origin.x + target.x) / 2, y: (origin.y + target.y) / 2)
        var arrow = Path()
        arrow.move(to: CGPoint(x: marker.x - 10 * cos(angle - .pi / 6), y: marker.y - 10 * sin(angle - .pi / 6)))
        arrow.addLine(to: marker)
        arrow.addLine(to: CGPoint(x: marker.x - 10 * cos(angle + .pi / 6), y: marker.y - 10 * sin(angle + .pi / 6)))
        context.stroke(arrow, with: .color(.secondary), lineWidth: 1.5)
    }
}
