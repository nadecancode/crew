import SwiftUI

/// Native containers with composition hooks for later Rust-backed content.
/// Adaptive layout changes reuse the same navigation paths and selection.
@MainActor
public struct CrewShell<Content: View, WorkspaceControl: View, Utilities: View>: View {
    @Bindable private var navigation: CrewNavigation
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    @State private var columnVisibility: NavigationSplitViewVisibility = .automatic
    @FocusState private var destinationFocus: CrewDestination?
    @AccessibilityFocusState private var readingDestination: CrewDestination?
    private let focus: CrewFocus?
    private let content: (CrewDestination) -> Content
    private let workspace: () -> WorkspaceControl
    private let utilities: () -> Utilities

    public init(
        navigation: CrewNavigation,
        focus: CrewFocus? = nil,
        @ViewBuilder content: @escaping (CrewDestination) -> Content,
        @ViewBuilder workspace: @escaping () -> WorkspaceControl,
        @ViewBuilder utilities: @escaping () -> Utilities
    ) {
        self.navigation = navigation
        self.focus = focus
        self.content = content
        self.workspace = workspace
        self.utilities = utilities
    }

    public var body: some View {
        Group {
            #if os(iOS)
            if horizontalSizeClass == .regular && !dynamicTypeSize.isAccessibilitySize {
                splitNavigation
            } else {
                tabNavigation
            }
            #else
            splitNavigation
                .background {
                    GeometryReader { geometry in
                        Color.clear
                            .onChange(of: geometry.size.width, initial: true) { _, width in
                                if width < 700 { columnVisibility = .detailOnly }
                            }
                    }
                }
            #endif
        }
        .tint(CrewPalette.accent)
        .background(CrewPalette.canvas)
        .focusedSceneValue(\.crewNavigation, navigation)
    }

    #if os(iOS)
    private var tabNavigation: some View {
        TabView(selection: $navigation.selection) {
            ForEach(CrewDestination.allCases) { destination in
                destinationStack(destination)
                    .tabItem {
                        Label(destination.title, systemImage: destination.symbol)
                    }
                    .tag(destination)
            }
        }
    }
    #endif

    private var splitNavigation: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            List(CrewDestination.allCases, selection: navigation.sidebarSelection) { destination in
                Label(destination.title, systemImage: destination.symbol)
                    .accessibilityIdentifier("crew.destination.\(destination.rawValue)")
                .tag(destination)
            }
            .listStyle(.sidebar)
            .navigationTitle("Crew")
            .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 320)
        } detail: {
            destinationStack(navigation.selection)
        }
        .navigationSplitViewStyle(.balanced)
    }

    private func destinationStack(_ destination: CrewDestination) -> some View {
        NavigationStack(path: navigation.pathBinding(for: destination)) {
            content(destination)
                .navigationTitle(destination.title)
                .toolbar {
                    ToolbarItem(placement: .principal) {
                        Button(destination.title) { navigation.selection = destination }
                            .focused($destinationFocus, equals: destination)
                            .accessibilityFocused($readingDestination, equals: destination)
                            .accessibilityLabel("\(destination.title) destination")
                            .accessibilityIdentifier("crew.current-destination")
                            #if os(iOS)
                            .frame(minWidth: 44, minHeight: 44)
                            #endif
                    }
                    shellToolbar
                }
                .onChange(of: focus?.request?.id) { _, _ in
                    guard navigation.selection == destination, let request = focus?.request,
                          request.destination == destination, request.target == nil else { return }
                    destinationFocus = destination
                    readingDestination = destination
                    focus?.consume(request.id)
                }
        }
        .id(destination)
        .accessibilityIdentifier("crew.content.\(destination.rawValue)")
    }

    @ToolbarContentBuilder
    private var shellToolbar: some ToolbarContent {
        #if os(iOS)
        ToolbarItem(placement: .topBarLeading) { workspace() }
        ToolbarItem(placement: .topBarTrailing) { utilities() }
        #else
        ToolbarItem(placement: .navigation) { workspace() }
        ToolbarItemGroup(placement: .primaryAction) {
            // Available even when a narrow window hides the sidebar.
            Menu {
                ForEach(CrewDestination.allCases) { destination in
                    Button {
                        navigation.selection = destination
                    } label: {
                        Label(destination.title, systemImage: destination.symbol)
                    }
                }
            } label: {
                Label("Destinations", systemImage: "sidebar.left")
            }
            .help("Choose a destination")
            .accessibilityIdentifier("crew.destinations")
            utilities()
        }
        #endif
    }
}
