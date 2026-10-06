import CrewBindings
import Observation
import SwiftUI

/// Per-window presentation state. No authentication, domain data, or drafts
/// are persisted here. Connected content supplies its own typed destinations.
@MainActor
@Observable
public final class CrewNavigation {
    public var selection: CrewDestination = .team
    public var presentedSheet: CrewShellSheet?
    public var inspectorVisible = true
    private var paths: [CrewDestination: NavigationPath] = [:]
    private var evidence: [CrewDestination: ResourceKey] = [:]
    private var evidenceDepth: [CrewDestination: Int] = [:]

    public init() {}

    public func path(for destination: CrewDestination) -> NavigationPath {
        paths[destination] ?? NavigationPath()
    }

    public func replacePath(_ path: NavigationPath, for destination: CrewDestination) {
        paths[destination] = path
        if let depth = evidenceDepth[destination], path.count < depth { closeEvidence(for: destination) }
    }

    public func pathBinding(for destination: CrewDestination) -> Binding<NavigationPath> {
        Binding(
            get: { self.path(for: destination) },
            set: { self.replacePath($0, for: destination) }
        )
    }

    public func resetPaths() { paths.removeAll(); evidence.removeAll(); evidenceDepth.removeAll() }

    public func openEvidence(for destination: CrewDestination) -> ResourceKey? { evidence[destination] }

    public func appendEvidence(_ key: ResourceKey, for destination: CrewDestination) {
        var path = path(for: destination)
        path.append(key)
        paths[destination] = path
        evidence[destination] = key
        evidenceDepth[destination] = path.count
    }

    public func presentEvidenceInInspector(_ key: ResourceKey, for destination: CrewDestination) {
        evidence[destination] = key
        evidenceDepth[destination] = nil
    }

    public func closeEvidence(for destination: CrewDestination) {
        evidence[destination] = nil
        evidenceDepth[destination] = nil
    }

    /// Physical stack transfer is not Back. Keep only the open evidence route.
    public func transferToInspector(for destination: CrewDestination) {
        paths[destination] = NavigationPath()
        evidenceDepth[destination] = nil
    }

    public func transferToCompact(entity: EntityKey, for destination: CrewDestination) {
        var path = NavigationPath()
        path.append(entity)
        if let resource = evidence[destination] {
            path.append(resource)
            evidenceDepth[destination] = path.count
        }
        paths[destination] = path
    }

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
