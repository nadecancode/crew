#if os(macOS)
import CrewUI
import SwiftUI

struct CrewNavigationCommands: Commands {
    @FocusedValue(\.crewNavigation) private var navigation

    var body: some Commands {
        CommandMenu("Navigate") {
            ForEach(Array(CrewDestination.allCases.enumerated()), id: \.element) { index, destination in
                Button(destination.title) { navigation?.selection = destination }
                    .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
                    .disabled(navigation == nil)
            }
            Divider()
            Button("Connections") { navigation?.presentedSheet = .connections }
                .keyboardShortcut(",", modifiers: [.command, .shift])
                .disabled(navigation == nil)
        }
        CommandGroup(replacing: .help) {
            Button("Crew Help") { navigation?.presentedSheet = .help }
                .disabled(navigation == nil)
        }
    }
}
#endif
