// swift-tools-version: 5.10
import PackageDescription

// This package contains presentation only. CrewClient is linked by the apps
// when the real generated bridge and matching XCFramework are available.
let package = Package(
    name: "CrewUI",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "CrewUI", targets: ["CrewUI"])],
    dependencies: [
        .package(path: "../CrewClient"),
        .package(url: "https://github.com/migueldeicaza/SwiftTerm.git", exact: "1.19.0")
    ],
    targets: [
        .target(
            name: "CrewUI",
            dependencies: [
                .product(name: "CrewBindings", package: "CrewClient"),
                .product(name: "SwiftTerm", package: "SwiftTerm")
            ],
            resources: [.process("Resources")]
        )
    ],
    swiftLanguageVersions: [.v5]
)
