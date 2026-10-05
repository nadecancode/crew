import CrewUI
import SwiftUI

@MainActor
struct CrewRootView: View {
    @State private var navigation = CrewNavigation()
    @AppStorage(CrewPreferences.appearanceKey) private var appearanceRaw = CrewAppearance.system.rawValue
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    #endif

    var body: some View {
        CrewShell(navigation: navigation) { destination in
            // Native-apple-ui replaces this composition with live typed views.
            CrewUnconnectedView(destination: destination, openConnections: showConnections)
        } workspace: {
            Button(action: showConnections) {
                Label("No workspace", systemImage: "square.stack.3d.up")
                    #if os(iOS)
                    .frame(minWidth: 44, minHeight: 44)
                    #endif
            }
            .accessibilityLabel("No workspace selected. Open Connections")
            .accessibilityIdentifier("crew.workspace")
        } utilities: {
            Menu {
                Button("Connections", systemImage: "network", action: showConnections)
                #if os(iOS)
                Button("Appearance", systemImage: "circle.lefthalf.filled") {
                    navigation.presentedSheet = .appearance
                }
                #else
                SettingsLink { Label("Settings", systemImage: "gearshape") }
                #endif
                Button("Help", systemImage: "questionmark.circle") {
                    navigation.presentedSheet = .help
                }
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
            .help("Connections, appearance and help")
        }
        .preferredColorScheme(CrewAppearance(rawValue: appearanceRaw)?.colorScheme)
        .sheet(item: $navigation.presentedSheet) { sheet in
            CrewLocalSheet(sheet: sheet)
        }
        #if os(macOS)
        .frame(minWidth: 320, minHeight: 360)
        #endif
    }

    private func showConnections() {
        #if os(macOS)
        openWindow(id: "connections")
        #else
        navigation.presentedSheet = .connections
        #endif
    }
}

struct CrewLocalSheet: View {
    let sheet: CrewShellSheet
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                switch sheet {
                case .connections: CrewConnectionsUnavailableView()
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

#if os(macOS)
struct CrewConnectionsWindow: View {
    @AppStorage(CrewPreferences.appearanceKey) private var appearanceRaw = CrewAppearance.system.rawValue
    @Environment(\.dismissWindow) private var dismissWindow

    var body: some View {
        NavigationStack {
            CrewConnectionsUnavailableView()
                .navigationTitle("Connections")
                .toolbar {
                    Button("Close") { dismissWindow(id: "connections") }
                        .keyboardShortcut(.cancelAction)
                        .accessibilityIdentifier("crew.connections-close")
                }
        }
        .preferredColorScheme(CrewAppearance(rawValue: appearanceRaw)?.colorScheme)
        .tint(CrewPalette.accent)
        .frame(minWidth: 320, idealWidth: 560, minHeight: 360, idealHeight: 520)
    }
}
#endif
