import SwiftUI

public struct CrewUnconnectedView: View {
    private let destination: CrewDestination
    private let openConnections: () -> Void

    public init(destination: CrewDestination, openConnections: @escaping () -> Void) {
        self.destination = destination
        self.openConnections = openConnections
    }

    public var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                ContentUnavailableView {
                    Label("Connect to Crew", systemImage: destination.symbol)
                } description: {
                    Text(destination.unconnectedDescription)
                } actions: {
                    Button(action: openConnections) {
                        Text("Connections").frame(minWidth: 44, minHeight: 44)
                    }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("crew.open-connections")
                }
                Label("Not connected · No workspace selected", systemImage: "network.slash")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier("crew.unconnected-status")
            }
            .frame(maxWidth: 640)
            .padding(24)
            .frame(maxWidth: .infinity)
        }
        .background(CrewPalette.canvas)
    }
}

/// Honest foundation state. This view neither authenticates nor fabricates a
/// connection. The later connection service supplies real setup presentation.
public struct CrewConnectionsUnavailableView: View {
    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                ContentUnavailableView {
                    Label("No connections", systemImage: "network")
                } description: {
                    Text("Connection setup is currently unavailable. No host is connected and no workspace has been loaded.")
                }
                Text("Connecting to Crew")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
                #if os(iOS)
                Text("iPhone and iPad connect through a scoped Crew gateway. Ask the host operator for its public host profile and independently verify its trust information.")
                #else
                Text("A Mac can use a protected connection to Crew on this computer or a scoped Crew gateway. A connection must be authenticated before workspace access is available.")
                #endif
                Text("Adding a connection does not start a team or expose a server. Never paste or scan crew web output into an app or page.")
                    .font(.callout)
                Text("Workspace access does not create an OS sandbox. Control and Terminal access can direct agents to act with their existing host permissions.")
                    .font(.callout)
            }
            .textSelection(.enabled)
            .frame(maxWidth: 640, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity)
        }
        .background(CrewPalette.canvas)
        .accessibilityIdentifier("crew.connections-unavailable")
    }
}
