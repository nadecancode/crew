import Foundation
import XCTest
#if os(macOS)
@testable import CrewMac
#elseif os(iOS)
@testable import CrewIOS
#endif

final class CrewExternalLinksTests: XCTestCase {
    func testAbsoluteHTTPAndHTTPSWithHostAreAccepted() {
        XCTAssertNotNil(CrewExternalLinks.preview("http://example.test/path"))
        XCTAssertNotNil(CrewExternalLinks.preview("HTTPS://example.test/path?q=1#fragment"))
    }

    func testOtherSchemesRelativeUserinfoAndWhitespaceAreRejected() {
        for value in ["javascript:alert(1)", "file:///tmp/example", "data:text/plain,example", "mailto:a@example.test", "/relative", "https://", "http:/path", "https://user:password@example.test/", "https://@example.test/", " https://example.test", "https://example.test/a b", "https://example.test/\n", "https://example.test/\u{0000}"] {
            XCTAssertNil(CrewExternalLinks.preview(value), value)
        }
    }

    func testDetectedTextPunctuationTrimMatchesLinkPieces() {
        XCTAssertEqual(CrewExternalLinks.preview("https://example.test/path.,;:!?)]}", detectedText: true)?.absoluteString, "https://example.test/path")
        XCTAssertEqual(CrewExternalLinks.preview("https://example.test/path)")?.absoluteString, "https://example.test/path)")
    }

    func testNoOpenerRunsWithoutTheExplicitGesture() throws {
        let url = try XCTUnwrap(CrewExternalLinks.preview("https://example.test/path"))
        var opened: [URL] = []
        XCTAssertFalse(CrewExternalLinks.open(url, explicitGesture: false) { opened.append($0) })
        XCTAssertTrue(opened.isEmpty)
        XCTAssertTrue(CrewExternalLinks.open(url, explicitGesture: true) { opened.append($0) })
        XCTAssertEqual(opened, [url])
    }
}
