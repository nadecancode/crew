import XCTest

/// Source for the Apple runner; these tests have not run on the Linux host.
final class CrewShellUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testFiveDestinationsKeepAnHonestUnconnectedState() {
        let app = XCUIApplication()
        app.launchArguments = ["-crew.appearance", "system"]
        app.launch()
        let destinations = ["Team", "Tasks", "Messages", "Events", "Usage"]
        let descriptions = [
            "Connect to a host and select a workspace to see its agents and current work.",
            "Connect to a host and select a workspace to read its tasks and dependencies.",
            "Connect to a host and select a workspace to read its messages and questions.",
            "Connect to a host and select a workspace to read its events and evidence.",
            "Connect to a host and select a workspace to see reported usage."
        ]
        for (index, title) in destinations.enumerated() {
            #if os(macOS)
            app.menuBars.menuBarItems["Navigate"].click()
            app.menuItems[title].click()
            #else
            let tab = app.tabBars.buttons[title]
            if tab.exists { tab.tap() }
            else {
                app.descendants(matching: .any)
                    .matching(identifier: "crew.destination.\(title.lowercased())")
                    .firstMatch.tap()
            }
            #endif
            XCTAssertTrue(app.staticTexts["crew.unconnected-status"].waitForExistence(timeout: 5))
            XCTAssertTrue(app.staticTexts[descriptions[index]].waitForExistence(timeout: 5))
        }
    }

    func testConnectionsExplainsUnavailableSetupWithoutAuthenticating() {
        let app = XCUIApplication()
        app.launch()
        let open = app.buttons["crew.open-connections"].firstMatch
        XCTAssertTrue(open.waitForExistence(timeout: 5))
        #if os(macOS)
        open.click()
        #else
        open.tap()
        #endif
        XCTAssertTrue(app.staticTexts["No connections"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Connection setup is currently unavailable. No host is connected and no workspace has been loaded."].exists)
        #if os(macOS)
        app.buttons["crew.connections-close"].click()
        #else
        app.buttons["crew.sheet-done"].tap()
        #endif
        XCTAssertTrue(open.waitForExistence(timeout: 5))
    }
}
