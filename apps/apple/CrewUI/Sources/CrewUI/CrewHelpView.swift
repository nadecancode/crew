import SwiftUI

public struct CrewHelpView: View {
    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Crew")
                    .font(.largeTitle.bold())
                    .accessibilityAddTraits(.isHeader)
                Text("Use Team, Tasks, Messages, Events and Usage to move between your workspace's work and evidence. Connections, appearance and help live outside those destinations.")
                Text("Connection and workspace")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                Text("Crew is not connected until a host is authenticated. A connected host and an available workspace are separate facts. Select a workspace before reading its data or taking an action.")
                Text("Closing Crew")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                Text("Closing a window or quitting this app does not stop a team. Disconnect and Stop team are separate operations.")
                #if os(macOS)
                Text("Keyboard navigation")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                Text("Use Command-1 through Command-5 to select destinations. The Navigate and View menus keep navigation available when the sidebar is hidden. Command-comma opens settings.")
                #endif
            }
            .textSelection(.enabled)
            .frame(maxWidth: 640, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity)
        }
        .background(CrewPalette.canvas)
    }
}
