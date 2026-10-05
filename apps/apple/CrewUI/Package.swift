// swift-tools-version: 5.10
import PackageDescription

// This package contains presentation only. CrewClient is linked by the apps
// when the real generated bridge and matching XCFramework are available.
let package = Package(
    name: "CrewUI",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "CrewUI", targets: ["CrewUI"])],
    targets: [
        .target(name: "CrewUI", resources: [.process("Resources")])
    ],
    swiftLanguageVersions: [.v5]
)
