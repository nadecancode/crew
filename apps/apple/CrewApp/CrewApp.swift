import SwiftUI

@main
struct CrewApp: App {
    var body: some Scene {
        #if os(macOS)
        WindowGroup("Crew") {
            CrewRootView()
        }
        .defaultSize(width: 1100, height: 740)
        .commands {
            SidebarCommands()
            CrewNavigationCommands()
        }

        Window("Connections", id: "connections") {
            CrewConnectionsWindow()
        }
        .defaultSize(width: 560, height: 520)

        Settings {
            CrewAppearanceSettings()
                .frame(minWidth: 360, idealWidth: 460, minHeight: 200)
        }
        #else
        WindowGroup("Crew") {
            CrewRootView()
        }
        #endif
    }
}
