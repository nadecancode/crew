import CrewBindings
import CrewUI
import SwiftUI

@MainActor
struct CrewRootView: View {
    @State private var navigation = CrewNavigation()
    @State private var model = CrewSessionModel()
    @State private var actionSheet: CrewActionSheet?
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage(CrewPreferences.appearanceKey) private var appearanceRaw = CrewAppearance.system.rawValue

    var body: some View {
        CrewShell(navigation: navigation) { destination in
            destinationContent(destination)
                .navigationDestination(for: EntityKey.self) { key in
                    detailContent(key)
                        .navigationTitle(detailTitle(key))
                        .task { model.select(key) }
                }
                .navigationDestination(for: ResourceKey.self) { key in
                    resourceContent(key)
                        .navigationTitle("Evidence")
                        .onDisappear { model.closeResource(key) }
                }
        } workspace: {
            Menu {
                if let listing = model.workspaces {
                    ForEach(listing.joined, id: \.id) { workspace in
                        Button(workspace.project ?? workspace.workspace) { model.selectWorkspace(workspace.id) }
                    }
                }
                Button("Connections", systemImage: "network", action: showConnections)
            } label: {
                Label(model.session?.project ?? model.session?.workspace ?? "No workspace", systemImage: "square.stack.3d.up")
                    #if os(iOS)
                    .frame(minWidth: 44, minHeight: 44)
                    #endif
            }
            .accessibilityLabel("Workspace. \(model.session?.workspace ?? "No workspace selected")")
            .accessibilityIdentifier("crew.workspace")
        } utilities: {
            Menu {
                if let scope = model.scope {
                    Button("Compose message", systemImage: "square.and.pencil") {
                        actionSheet = .compose(EntityKey(scope: scope, entity: .workspace))
                    }
                    Button("Continue paused agents") { model.perform(.resumeLimited(scope: scope, restart: false)) }
                    Button("Restart paused agents") { model.perform(.resumeLimited(scope: scope, restart: true)) }
                    Button("Stop team", role: .destructive) { model.perform(.stopTeam(scope: scope)) }
                }
                Button("Connections", systemImage: "network", action: showConnections)
                #if os(iOS)
                Button("Appearance", systemImage: "circle.lefthalf.filled") { navigation.presentedSheet = .appearance }
                #else
                SettingsLink { Label("Settings", systemImage: "gearshape") }
                #endif
                Button("Help", systemImage: "questionmark.circle") { navigation.presentedSheet = .help }
                #if os(iOS)
                Section("Navigate") {
                    ForEach(Array(CrewDestination.allCases.enumerated()), id: \.element) { index, destination in
                        Button(destination.title) { navigation.selection = destination }
                            .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
                    }
                }
                #endif
            } label: {
                Label("Crew options", systemImage: "ellipsis.circle")
                    .labelStyle(.iconOnly)
                    #if os(iOS)
                    .frame(minWidth: 44, minHeight: 44)
                    #endif
            }
            .accessibilityLabel("Crew options")
            .accessibilityIdentifier("crew.options")
            .help("Actions, connections, appearance and help")
        }
        .preferredColorScheme(CrewAppearance(rawValue: appearanceRaw)?.colorScheme)
        .sheet(item: $navigation.presentedSheet) { sheet in
            CrewLocalSheet(sheet: sheet, model: model)
        }
        .sheet(item: $actionSheet) { sheet in CrewActionSheetView(sheet: sheet, model: model) }
        .alert(model.confirmation?.title ?? "Confirm action", isPresented: Binding(
            get: { model.confirmation != nil },
            set: { if !$0 { model.cancelConfirmation() } }
        ), presenting: model.confirmation) { confirmation in
            Button("Cancel", role: .cancel) { model.cancelConfirmation() }.keyboardShortcut(.defaultAction)
            Button("Confirm", role: .destructive) { model.confirm(confirmation) }
        } message: { confirmation in Text(verbatim: confirmation.consequence) }
        .onChange(of: navigation.selection, initial: true) { _, destination in model.destination = destination }
        .onChange(of: model.scope) { _, _ in
            navigation.resetPaths()
            actionSheet = nil
        }
        .onChange(of: scenePhase, initial: true) { _, phase in model.setActive(phase == .active) }
        .task {
            #if os(macOS)
            await model.discoverLocal()
            #endif
        }
        .onDisappear { _Concurrency.Task { await model.close() } }
        #if os(macOS)
        .frame(minWidth: 320, minHeight: 360)
        #endif
    }

    @ViewBuilder private func destinationContent(_ destination: CrewDestination) -> some View {
        if let view = model.views[destination], let scope = view.scope {
            VStack(spacing: 0) {
                status(view)
                questionBanner(scope)
                Group {
                    if view.freshness == .loading {
                        ProgressView("Synchronizing this workspace")
                    } else if view.freshness == .unavailable {
                        ContentUnavailableView("Workspace content is unavailable", systemImage: "network.slash")
                    } else { switch view.payload {
                    case .team(let data): CrewTeamList(scope: scope, data: data, taskTitle: { id in model.tasks?.tasks.first { $0.task.id == id }?.task.title }, select: select)
                    case .tasks(let data): CrewTaskList(scope: scope, data: data, select: select)
                    case .messages(let page):
                        CrewMessageList(scope: scope, page: page, select: select, loadOlder: model.loadOlderMessages)
                            .toolbar { Button("Newest", action: model.newestMessages) }
                    case .events(let page):
                        CrewEventList(scope: scope, page: page, select: select, loadOlder: model.loadOlderEvents)
                            .toolbar { Button("Newest", action: model.newestEvents) }
                    case .usage(let data): CrewUsageView(usage: data)
                    default: ContentUnavailableView("This view is unavailable", systemImage: destination.symbol)
                    }
                    }
                }
                if let feedback = model.feedback { Text(verbatim: feedback).font(.callout).padding(12) }
            }
            .refreshable { model.refresh() }
        } else if let notice = model.notice, notice.endpoint != .disconnected {
            ContentUnavailableView {
                Label(CrewFailure.workspace(notice.workspace), systemImage: "network")
            } description: {
                if let reason = notice.reason { Text(verbatim: reason.message) }
                else { Text("Authentication and the workspace snapshot must complete before content appears.") }
            } actions: {
                Button("Connections", action: showConnections)
                Button("Refresh", action: model.refresh)
            }
        } else {
            CrewUnconnectedView(destination: destination, openConnections: showConnections)
        }
    }

    private func status(_ view: ViewData) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(model.presentationStatus, systemImage: model.isLive ? "checkmark.circle" : "network.slash")
                Spacer()
                Button("Refresh", systemImage: "arrow.clockwise", action: model.refresh).labelStyle(.iconOnly)
            }
            if !model.isLive {
                Text("Last known. Actions require a synchronized live connection.").font(.callout)
                if let at = view.synchronizedAtMs { Text("Last synchronized \(CrewLabels.timestamp(at))").font(.caption) }
            }
            if let error = model.error { Text(verbatim: error).foregroundStyle(CrewPalette.danger) }
        }
        .font(.callout)
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CrewPalette.surface)
    }

    @ViewBuilder private func questionBanner(_ scope: ClientScope) -> some View {
        if case .questions(let messages) = model.questions?.payload, !messages.isEmpty {
            Menu {
                ForEach(messages, id: \.id) { message in
                    Button("Answer question from \(message.from)") {
                        _Concurrency.Task {
                            do {
                                let info = try await model.questionInfo(message)
                                guard model.scope == scope else { return }
                                actionSheet = .question(EntityKey(scope: scope, entity: .message(message.id)), message, info)
                            } catch { model.showError(error) }
                        }
                    }
                }
            } label: {
                Label("\(messages.count) questions need an answer", systemImage: "questionmark.bubble")
                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            }
            .padding(.horizontal, 12)
        }
    }

    @ViewBuilder private func detailContent(_ key: EntityKey) -> some View {
        if key.scope != model.scope {
            ContentUnavailableView("This workspace is no longer available", systemImage: "network.slash")
        } else if model.selected == key {
            if let resource = model.resource, case .evidence(let data) = resource.payload,
               model.selectedResource?.entity == key, key.entity.isReadEntity {
                CrewResourceView(resource: data, refresh: model.refreshResource, copy: CrewClipboard.copy)
            } else if let detail = model.detail {
                switch detail.payload {
                case .agentDetail(let data):
                    CrewAgentDetail(scope: key.scope, data: data, pending: model.pending, select: select, perform: model.perform, evidence: showEvidence, compose: { actionSheet = .compose($0) }, prompt: { actionSheet = .prompt($0) }, attach: { model.attach($0); actionSheet = .terminal($0) }, copy: CrewClipboard.copy)
                case .taskDetail(let data):
                    if let task = data.task { CrewTaskDetail(scope: key.scope, task: task, select: select, copy: CrewClipboard.copy) }
                    else { CrewResourceView(resource: data.card, refresh: model.refreshResource, copy: CrewClipboard.copy) }
                default: ContentUnavailableView("Detail unavailable", systemImage: "doc.text")
                }
            } else {
                ProgressView("Loading detail")
            }
        } else {
            ProgressView("Loading selected entity")
        }
    }

    private func select(_ key: EntityKey) {
        model.select(key)
        navigation.pathBinding(for: navigation.selection).wrappedValue.append(key)
    }

    private func showEvidence(_ key: ResourceKey) {
        model.readResource(key)
        navigation.pathBinding(for: navigation.selection).wrappedValue.append(key)
    }

    @ViewBuilder private func resourceContent(_ key: ResourceKey) -> some View {
        if key.entity.scope != model.scope {
            ContentUnavailableView("Evidence is no longer available", systemImage: "network.slash")
        } else if model.selectedResource == key, case .evidence(let data) = model.resource?.payload {
            if key.kind == .models, case .models(let models) = data.value {
                CrewModelChoicesView(key: key.entity, models: models, model: model)
            } else {
                CrewResourceView(resource: data, refresh: model.refreshResource, copy: CrewClipboard.copy)
            }
        } else {
            ProgressView("Loading scoped evidence")
        }
    }

    private func detailTitle(_ key: EntityKey) -> String {
        switch key.entity {
        case .agent(let id), .task(let id), .message(let id): return id
        case .event(let seq): return "Event \(seq)"
        case .workspace: return "Workspace"
        }
    }

    private func showConnections() { navigation.presentedSheet = .connections }
}

private extension Entity {
    var isReadEntity: Bool {
        switch self { case .message, .event: return true; default: return false }
    }
}

@MainActor
struct CrewLocalSheet: View {
    let sheet: CrewShellSheet
    @Bindable var model: CrewSessionModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                switch sheet {
                case .connections: CrewConnectionsView(model: model)
                case .appearance: CrewAppearanceSettings()
                case .help: CrewHelpView()
                }
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button { dismiss() } label: {
                        Text("Done")
                            #if os(iOS)
                            .frame(minWidth: 44, minHeight: 44)
                            #endif
                    }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("crew.sheet-done")
                }
            }
        }
        .tint(CrewPalette.accent)
        #if os(macOS)
        .frame(minWidth: 360, idealWidth: 560, minHeight: 360, idealHeight: 520)
        #endif
    }

    private var title: String {
        switch sheet {
        case .connections: return "Connections"
        case .appearance: return "Appearance"
        case .help: return "Crew Help"
        }
    }
}
