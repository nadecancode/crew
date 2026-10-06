import CrewBindings
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

    func testFocusRestoresRenderedOpenerBeforeClosedEntity() {
        let focus = CrewFocus()
        let opener = EntityKey(scope: syntheticScope, entity: .agent("synthetic-agent"))
        let closed = EntityKey(scope: syntheticScope, entity: .task("synthetic-task"))
        focus.restore(opener: opener, closed: closed, rendered: [opener, closed], destination: .team)
        XCTAssertEqual(focus.request?.target, opener)
        focus.restore(opener: opener, closed: closed, rendered: [closed], destination: .tasks)
        XCTAssertEqual(focus.request?.target, closed)
    }

    func testMissingOrRetiredEntityFocusFallsBackToCurrentDestination() {
        let focus = CrewFocus()
        let closed = EntityKey(scope: syntheticScope, entity: .task("synthetic-task"))
        let changed = ClientScope(endpointId: syntheticScope.endpointId, workspaceId: syntheticScope.workspaceId, generation: syntheticScope.generation, daemonPid: syntheticScope.daemonPid, daemonStartedAtMs: syntheticScope.daemonStartedAtMs, attempt: syntheticScope.attempt + 1)
        let row = EntityKey(scope: changed, entity: closed.entity)
        focus.restore(opener: closed, closed: closed, rendered: [row], destination: .tasks)
        XCTAssertNil(focus.request?.target)
        XCTAssertEqual(focus.request?.destination, .tasks)
    }

    func testConsumedFocusRequestCannotReplayAfterLayoutOrScopeChange() throws {
        let focus = CrewFocus()
        focus.restore(opener: nil, closed: nil, rendered: [], destination: .events)
        let old = try XCTUnwrap(focus.request?.id)
        focus.restore(opener: nil, closed: nil, rendered: [], destination: .tasks)
        let current = try XCTUnwrap(focus.request?.id)
        focus.consume(old)
        XCTAssertEqual(focus.request?.id, current)
        focus.consume(current)
        XCTAssertNil(focus.request)
        focus.restore(opener: nil, closed: nil, rendered: [], destination: .team)
        focus.cancel()
        XCTAssertNil(focus.request)
    }

    func testInspectorPreferenceIsIndependentPerWindow() {
        let first = CrewNavigation()
        let second = CrewNavigation()
        first.inspectorVisible = false
        XCTAssertTrue(second.inspectorVisible)
    }

    func testEvidenceBackCannotResurrectTheRouteWhenWidening() {
        let navigation = CrewNavigation()
        let entity = EntityKey(scope: syntheticScope, entity: .agent("synthetic-agent"))
        let resource = ResourceKey(entity: entity, kind: .transcript, options: .bounded(limit: 100))
        navigation.pathBinding(for: .team).wrappedValue.append(entity)
        navigation.appendEvidence(resource, for: .team)
        XCTAssertEqual(navigation.openEvidence(for: .team), resource)
        var back = navigation.path(for: .team)
        back.removeLast()
        navigation.replacePath(back, for: .team)
        navigation.transferToInspector(for: .team)
        XCTAssertNil(navigation.openEvidence(for: .team))
        XCTAssertTrue(navigation.path(for: .team).isEmpty)
    }

    func testOpenEvidenceTransfersWithoutBeingTreatedAsBack() {
        let navigation = CrewNavigation()
        let entity = EntityKey(scope: syntheticScope, entity: .agent("synthetic-agent"))
        let resource = ResourceKey(entity: entity, kind: .diff, options: .diff(against: nil))
        navigation.pathBinding(for: .team).wrappedValue.append(entity)
        navigation.appendEvidence(resource, for: .team)
        navigation.transferToInspector(for: .team)
        XCTAssertEqual(navigation.openEvidence(for: .team), resource)
        navigation.transferToCompact(entity: entity, for: .team)
        XCTAssertEqual(navigation.path(for: .team).count, 2)
        XCTAssertEqual(navigation.openEvidence(for: .team), resource)
    }

    func testActivatingEachWideDestinationNormalizesItsOwnCompactStack() {
        let navigation = CrewNavigation()
        let agent = EntityKey(scope: syntheticScope, entity: .agent("synthetic-agent"))
        let task = EntityKey(scope: syntheticScope, entity: .task("synthetic-task"))
        navigation.pathBinding(for: .team).wrappedValue.append(agent)
        navigation.pathBinding(for: .tasks).wrappedValue.append(task)
        navigation.transferToInspector(for: .tasks)
        XCTAssertEqual(navigation.path(for: .team).count, 1)
        navigation.selection = .team
        navigation.transferToInspector(for: .team)
        XCTAssertTrue(navigation.path(for: .team).isEmpty)
        XCTAssertTrue(navigation.path(for: .tasks).isEmpty)
        navigation.transferToCompact(entity: agent, for: .team)
        XCTAssertEqual(navigation.path(for: .team).count, 1)
    }

    private let syntheticScope = ClientScope(endpointId: "synthetic-endpoint", workspaceId: "synthetic-workspace", generation: 2, daemonPid: 123, daemonStartedAtMs: 1_700_000_000_000, attempt: 4)
}
