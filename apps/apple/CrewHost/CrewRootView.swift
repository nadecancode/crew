import CrewBindings
import CrewUI
import SwiftUI

@MainActor
struct CrewRootView: View {
    @State private var navigation = CrewNavigation()
    @State private var model = CrewSessionModel()
    @State private var focus = CrewFocus()
    @State private var selectionByDestination: [CrewDestination: EntityKey] = [:]
    @State private var openerByDestination: [CrewDestination: EntityKey] = [:]
    @State private var inspectorPath = NavigationPath()
    @State private var windowWidth: CGFloat = 0
    @State private var actionSheet: CrewActionSheet?
    @State private var teamGraph = false
    @State private var tasksGraph = false
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.scenePhase) private var scenePhase
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    @AppStorage(CrewPreferences.appearanceKey) private var appearanceRaw = CrewAppearance.system.rawValue

    var body: some View {
        GeometryReader { geometry in
            rootSurface
                .onChange(of: geometry.size.width, initial: true) { _, width in windowWidth = width }
        }
    }

    private var rootSurface: some View {
        CrewShell(navigation: navigation, focus: canRestoreFocus ? focus : nil) { destination in
            destinationContent(destination)
                .navigationDestination(for: EntityKey.self) { key in
                    detailContent(key)
                        .navigationTitle(detailTitle(key))
                        .task { model.select(key) }
                }
                .navigationDestination(for: ResourceKey.self) { key in
                    resourceContent(key)
                        .navigationTitle("Evidence")
                        .task(id: key) { activateResource(key) }
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
                Button(navigation.inspectorVisible ? "Hide inspector" : "Show inspector", systemImage: "sidebar.right") {
                    navigation.inspectorVisible.toggle()
                }
                .disabled(!usesInspector)
                if let scope = model.scope {
                    Button("Compose message", systemImage: "square.and.pencil") {
                        actionSheet = .compose(EntityKey(scope: scope, entity: .workspace))
                    }
                    workspaceAction("Continue agents paused at limit", .resumeLimited(scope: scope, restart: false))
                    workspaceAction("Restart agents paused at limit", .resumeLimited(scope: scope, restart: true))
                    workspaceAction("Stop team", .stopTeam(scope: scope), role: .destructive)
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
        .inspector(isPresented: Binding(
            get: { usesInspector && navigation.inspectorVisible },
            set: { if usesInspector { navigation.inspectorVisible = $0 } }
        )) {
            NavigationStack(path: $inspectorPath) {
                Group {
                    if let key = currentSelection {
                        detailContent(key)
                            .navigationTitle(detailTitle(key))
                            .task { model.select(key) }
                    } else {
                        ContentUnavailableView("Select an entity", systemImage: "sidebar.right", description: Text("Open an agent, task, message or event to read its details here."))
                    }
                }
                .navigationDestination(for: ResourceKey.self) { key in
                    resourceContent(key).navigationTitle("Evidence").task(id: key) { activateResource(key) }
                }
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close detail", systemImage: "xmark") { navigation.inspectorVisible = false }
                            .keyboardShortcut(.cancelAction)
                    }
                }
            }
            .inspectorColumnWidth(min: 340, ideal: 400, max: 720)
        }
        .preferredColorScheme(CrewAppearance(rawValue: appearanceRaw)?.colorScheme)
        .sheet(item: $navigation.presentedSheet) { sheet in
            CrewLocalSheet(sheet: sheet, model: model)
        }
        .sheet(item: $actionSheet) { sheet in CrewActionSheetView(sheet: sheet, model: model) }
        // Workspace-wide controls appear in menus and the usage view; their
        // exact intents are watched while this workspace scope is current.
        .crewAvailability(model, workspaceIntents)
        .alert(model.confirmation?.title ?? "Confirm action", isPresented: Binding(
            get: { model.confirmation != nil },
            set: { if !$0 { model.cancelConfirmation() } }
        ), presenting: model.confirmation) { confirmation in
            Button("Cancel", role: .cancel) { model.cancelConfirmation() }.keyboardShortcut(.defaultAction)
            Button("Confirm", role: .destructive) { model.confirm(confirmation) }
        } message: { confirmation in Text(verbatim: confirmation.consequence) }
        .onChange(of: navigation.selection, initial: true) { _, destination in
            model.destination = destination
            if usesInspector { adaptDetail(wide: true) }
            if usesInspector, navigation.inspectorVisible, let key = currentSelection { model.select(key) }
        }
        .onChange(of: usesInspector) { _, wide in adaptDetail(wide: wide) }
        .onChange(of: navigation.inspectorVisible) { _, visible in
            guard usesInspector else { return }
            if visible, let key = currentSelection { adaptDetail(wide: true); model.select(key) }
            else if !visible { closeDetailAndRestore() }
        }
        .onChange(of: model.selected) { old, selected in
            if let selected { selectionByDestination[navigation.selection] = selected }
            else if let old, old.scope == model.scope,
                    (!navigation.path(for: navigation.selection).isEmpty || (usesInspector && navigation.inspectorVisible)) {
                closeDetailAndRestore(closed: old)
            }
        }
        .onChange(of: navigation.path(for: navigation.selection).count) { old, count in
            if !usesInspector, old > 0, count == 0 { closeDetailAndRestore() }
        }
        .onChange(of: navigation.openEvidence(for: navigation.selection)) { old, current in
            if current == nil, let old { model.closeResource(old) }
        }
        .onChange(of: inspectorPath.count) { old, count in
            if usesInspector, old > 0, count == 0 { navigation.closeEvidence(for: navigation.selection) }
        }
        .onChange(of: model.scope) { _, _ in
            navigation.resetPaths()
            actionSheet = nil
            selectionByDestination = [:]
            openerByDestination = [:]
            inspectorPath = NavigationPath()
            focus.cancel()
        }
        .onChange(of: renderedKeys) { _, keys in
            if let target = focus.request?.target, !keys.contains(target) {
                focus.restore(opener: openerByDestination[navigation.selection], closed: currentSelection,
                              rendered: keys, destination: navigation.selection)
            }
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
        if let view = model.view(for: destination), let scope = view.scope {
            VStack(spacing: 0) {
                status(view)
                if case .overview(let data) = model.overview?.payload { CrewOverviewSummary(data: data, copy: CrewClipboard.copy) }
                questionBanner(scope)
                Group {
                    if view.freshness == .loading {
                        ProgressView("Synchronizing this workspace")
                    } else if view.freshness == .unavailable {
                        ContentUnavailableView("Workspace content is unavailable", systemImage: "network.slash")
                    } else { switch view.payload {
                    case .team(let data):
                        VStack(spacing: 0) {
                            graphPicker("Team presentation", selection: $teamGraph)
                            if teamGraph && !dynamicTypeSize.isAccessibilitySize {
                                CrewTeamGraph(scope: scope, team: data, select: selectFromRoot, pairMessages: { first, second in
                                    model.setMessageFilter(MessageFilter(endpoints: .pair(first: first, second: second), kind: nil, text: ""))
                                    navigation.selection = .messages
                                }, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty))
                            } else {
                                CrewTeamList(scope: scope, data: data, taskTitle: { id in model.tasks?.tasks.first { $0.task.id == id }?.task.title }, select: selectFromRoot, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty))
                            }
                        }
                    case .tasks(let data):
                        VStack(spacing: 0) {
                            graphPicker("Task presentation", selection: $tasksGraph)
                            if tasksGraph && !dynamicTypeSize.isAccessibilitySize { CrewTaskGraph(scope: scope, tasks: data, select: selectFromRoot, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty)) }
                            else { CrewTaskList(scope: scope, data: data, select: selectFromRoot, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty)) }
                        }
                    case .messages(let page):
                        VStack(spacing: 0) {
                            CrewMessageFilters(current: model.messageFilter, apply: model.setMessageFilter)
                            if model.loadingOlder(.messages) { ProgressView("Reading the bounded host history window") }
                            CrewMessageList(scope: scope, page: page, select: selectFromRoot, loadOlder: model.loadOlderMessages, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty))
                        }
                        .toolbar { Button("Newest", action: model.newestMessages) }
                    case .events(let page):
                        VStack(spacing: 0) {
                            CrewEventFilters(current: model.eventFilter, apply: model.setEventFilter)
                            if model.loadingOlder(.events) { ProgressView("Reading the bounded host event window") }
                            CrewEventList(scope: scope, page: page, select: selectFromRoot, loadOlder: model.loadOlderEvents, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty))
                        }
                        .toolbar { Button("Newest", action: model.newestEvents) }
                    case .usage(let data):
                        CrewUsageView(scope: scope, usage: data, selectAgent: { selectFromRoot(EntityKey(scope: scope, entity: .agent($0))) }, continueAvailability: model.availability(.resumeLimited(scope: scope, restart: false)), restartAvailability: model.availability(.resumeLimited(scope: scope, restart: true)), continueLimited: { model.perform(.resumeLimited(scope: scope, restart: false)) }, restartLimited: { model.perform(.resumeLimited(scope: scope, restart: true)) }, selected: openerByDestination[destination], focus: canRestoreFocus ? focus : nil, active: navigation.selection == destination && (usesInspector || navigation.path(for: destination).isEmpty))
                    default: ContentUnavailableView("This view is unavailable", systemImage: destination.symbol)
                    }
                    }
                }
                if let feedback = model.feedback { Text(verbatim: feedback).font(.callout).padding(12) }
            }
            .refreshable { model.refresh() }
        } else if model.notice?.workspace == .live, model.scope != nil {
            ProgressView("Loading \(destination.title)")
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

    private func graphPicker(_ title: String, selection: Binding<Bool>) -> some View {
        let displayed = Binding(
            get: { selection.wrappedValue && !dynamicTypeSize.isAccessibilitySize },
            set: { selection.wrappedValue = $0 }
        )
        return VStack(alignment: .leading, spacing: 8) {
        Picker(title, selection: displayed) {
            Text("List").tag(false)
            Text("Graph").tag(true)
        }
        .pickerStyle(.segmented)
        .padding(12)
        .disabled(dynamicTypeSize.isAccessibilitySize)
        if dynamicTypeSize.isAccessibilitySize {
            Text("List shown to keep all content readable at this text size.").font(.callout).padding(.horizontal, 12)
        }
        }
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
        selectionByDestination[navigation.selection] = key
        model.select(key)
        if usesInspector {
            navigation.closeEvidence(for: navigation.selection)
            inspectorPath = NavigationPath()
            navigation.inspectorVisible = true
        } else { navigation.pathBinding(for: navigation.selection).wrappedValue.append(key) }
    }

    private func selectFromRoot(_ key: EntityKey) {
        openerByDestination[navigation.selection] = key
        select(key)
    }

    private func showEvidence(_ key: ResourceKey) {
        model.readResource(key)
        if usesInspector {
            navigation.presentEvidenceInInspector(key, for: navigation.selection)
            inspectorPath.append(key)
        } else { navigation.appendEvidence(key, for: navigation.selection) }
    }

    private func activateResource(_ key: ResourceKey) {
        guard key.entity.scope == model.scope, navigation.openEvidence(for: navigation.selection) == key else { return }
        model.select(key.entity)
        if model.selectedResource != key { model.readResource(key) }
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

    private var canRestoreFocus: Bool { actionSheet == nil && navigation.presentedSheet == nil && model.confirmation == nil }

    private var currentSelection: EntityKey? {
        guard let key = selectionByDestination[navigation.selection], key.scope == model.scope else { return nil }
        return key
    }

    private var usesInspector: Bool {
        guard model.scope != nil, windowWidth >= 1_040, !dynamicTypeSize.isAccessibilitySize else { return false }
        #if os(iOS)
        return horizontalSizeClass == .regular
        #else
        return true
        #endif
    }

    private func adaptDetail(wide: Bool) {
        let evidence = navigation.openEvidence(for: navigation.selection)
        if wide {
            navigation.transferToInspector(for: navigation.selection)
            inspectorPath = NavigationPath()
            guard navigation.inspectorVisible, let key = currentSelection else { return }
            if let evidence, !key.entity.isReadEntity { inspectorPath.append(evidence) }
        } else if navigation.inspectorVisible, let key = currentSelection, navigation.path(for: navigation.selection).isEmpty {
            navigation.transferToCompact(entity: key, for: navigation.selection)
        }
    }

    private func closeDetailAndRestore(closed: EntityKey? = nil) {
        let key = closed ?? model.selected ?? currentSelection
        navigation.inspectorVisible = false
        navigation.replacePath(NavigationPath(), for: navigation.selection)
        inspectorPath = NavigationPath()
        model.closeDetail()
        focus.restore(opener: openerByDestination[navigation.selection], closed: key,
                      rendered: renderedKeys.subtracting(model.notice?.changes.removed ?? []), destination: navigation.selection)
    }

    private var workspaceIntents: [ActionIntent] {
        guard let scope = model.scope else { return [] }
        return [.resumeLimited(scope: scope, restart: false), .resumeLimited(scope: scope, restart: true), .stopTeam(scope: scope)]
    }

    /// A menu item cannot show a reason line, so the Core's reason is its help
    /// text and accessibility hint. Confirmation still comes from the Core.
    private func workspaceAction(_ title: String, _ intent: ActionIntent, role: ButtonRole? = nil) -> some View {
        let availability = model.availability(intent)
        return Button(title, role: role) { model.perform(intent) }
            .disabled(!availability.available)
            .help(availability.reason ?? title)
            .accessibilityHint(availability.reason ?? "")
    }

    private var renderedKeys: Set<EntityKey> {
        guard let view = model.view(for: navigation.selection), let scope = view.scope else { return [] }
        switch view.payload {
        case .team(let data): return Set(data.agents.map { EntityKey(scope: scope, entity: .agent($0.agent.id)) })
        case .tasks(let data): return Set(data.tasks.map { EntityKey(scope: scope, entity: .task($0.task.id)) })
        case .messages(let page): return Set(page.items.map { EntityKey(scope: scope, entity: .message($0.id)) })
        case .events(let page): return Set(page.items.compactMap { event in
            UInt64(exactly: event.seq).map { EntityKey(scope: scope, entity: .event($0)) }
        })
        case .usage(let data): return Set(data.agents.map { EntityKey(scope: scope, entity: .agent($0.agentId)) })
        default: return []
        }
    }
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
