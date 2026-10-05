import CrewUI
import SwiftUI
import XCTest

@MainActor
final class CrewShellTests: XCTestCase {
    func testDestinationsHaveStableUniqueIdentity() {
        XCTAssertEqual(CrewDestination.allCases.map(\.rawValue), ["team", "tasks", "messages", "events", "usage"])
        XCTAssertEqual(Set(CrewDestination.allCases.map(\.id)).count, 5)
    }

    func testEachDestinationKeepsItsPathWhenSelectionChanges() {
        let navigation = CrewNavigation()
        var teamPath = NavigationPath()
        teamPath.append("synthetic-team-route")
        var taskPath = NavigationPath()
        taskPath.append("synthetic-task-route")
        taskPath.append("synthetic-evidence-route")
        navigation.replacePath(teamPath, for: .team)
        navigation.replacePath(taskPath, for: .tasks)
        navigation.selection = .usage
        navigation.selection = .tasks
        XCTAssertEqual(navigation.path(for: .team).count, 1)
        XCTAssertEqual(navigation.path(for: .tasks).count, 2)
        XCTAssertTrue(navigation.path(for: .usage).isEmpty)
    }

    func testSidebarDeselectionDoesNotDiscardCurrentDestination() {
        let navigation = CrewNavigation()
        navigation.selection = .messages
        navigation.sidebarSelection.wrappedValue = nil
        XCTAssertEqual(navigation.selection, .messages)
        navigation.sidebarSelection.wrappedValue = .events
        XCTAssertEqual(navigation.selection, .events)
    }

    func testSheetPresentationDoesNotChangeDestinationOrPath() {
        let navigation = CrewNavigation()
        navigation.selection = .tasks
        navigation.pathBinding(for: .tasks).wrappedValue.append("synthetic-task-route")
        navigation.presentedSheet = .connections
        navigation.presentedSheet = nil
        XCTAssertEqual(navigation.selection, .tasks)
        XCTAssertEqual(navigation.path(for: .tasks).count, 1)
    }

    func testNewWindowsHaveIndependentNavigation() {
        let first = CrewNavigation()
        let second = CrewNavigation()
        first.selection = .events
        first.pathBinding(for: .events).wrappedValue.append("synthetic-event-route")
        XCTAssertEqual(second.selection, .team)
        XCTAssertTrue(second.path(for: .events).isEmpty)
    }

    func testAppearanceCanFollowTheSystem() {
        XCTAssertNil(CrewAppearance.system.colorScheme)
        XCTAssertEqual(CrewAppearance.light.colorScheme, .light)
        XCTAssertEqual(CrewAppearance.dark.colorScheme, .dark)
    }
}
