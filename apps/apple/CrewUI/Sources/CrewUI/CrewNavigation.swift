import Observation
import SwiftUI

/// Per-window presentation state. No authentication, domain data, or drafts
/// are persisted here. Connected content supplies its own typed destinations.
@MainActor
@Observable
public final class CrewNavigation {
    public var selection: CrewDestination = .team
    public var presentedSheet: CrewShellSheet?
    private var paths: [CrewDestination: NavigationPath] = [:]

    public init() {}

    public func path(for destination: CrewDestination) -> NavigationPath {
        paths[destination] ?? NavigationPath()
    }

    public func replacePath(_ path: NavigationPath, for destination: CrewDestination) {
        paths[destination] = path
    }

    public func pathBinding(for destination: CrewDestination) -> Binding<NavigationPath> {
        Binding(
            get: { self.path(for: destination) },
            set: { self.replacePath($0, for: destination) }
        )
    }

    public func resetPaths() { paths.removeAll() }

    public var sidebarSelection: Binding<CrewDestination?> {
        Binding(
            get: { self.selection },
            set: { if let destination = $0 { self.selection = destination } }
        )
    }
}

public enum CrewShellSheet: String, Identifiable {
    case connections, appearance, help
    public var id: String { rawValue }
}

public struct CrewNavigationFocusKey: FocusedValueKey {
    public typealias Value = CrewNavigation
}

public extension FocusedValues {
    var crewNavigation: CrewNavigation? {
        get { self[CrewNavigationFocusKey.self] }
        set { self[CrewNavigationFocusKey.self] = newValue }
    }
}
