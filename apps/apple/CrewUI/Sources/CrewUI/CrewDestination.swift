import SwiftUI

/// Local navigation identity, independent of workspace and domain entities.
public enum CrewDestination: String, CaseIterable, Hashable, Identifiable {
    case team, tasks, messages, events, usage

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .team: return "Team"
        case .tasks: return "Tasks"
        case .messages: return "Messages"
        case .events: return "Events"
        case .usage: return "Usage"
        }
    }

    public var symbol: String {
        switch self {
        case .team: return "person.3"
        case .tasks: return "checklist"
        case .messages: return "bubble.left.and.bubble.right"
        case .events: return "list.bullet.rectangle"
        case .usage: return "chart.bar"
        }
    }

    public var unconnectedDescription: String {
        switch self {
        case .team: return "Connect to a host and select a workspace to see its agents and current work."
        case .tasks: return "Connect to a host and select a workspace to read its tasks and dependencies."
        case .messages: return "Connect to a host and select a workspace to read its messages and questions."
        case .events: return "Connect to a host and select a workspace to read its events and evidence."
        case .usage: return "Connect to a host and select a workspace to see reported usage."
        }
    }
}
