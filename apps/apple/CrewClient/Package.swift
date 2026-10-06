// swift-tools-version: 5.10
import PackageDescription

// The actual matching XCFramework must be staged before package resolution or
// Xcode builds. A Linux library or an empty framework is not a replacement.
let package = Package(
    name: "CrewClient",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [.library(name: "CrewBindings", targets: ["CrewBindings"])],
    targets: [
        .binaryTarget(name: "CrewClientFFI", path: "Artifacts/CrewClientFFI.xcframework"),
        .target(
            name: "CrewBindings",
            dependencies: ["CrewClientFFI"],
            path: "Sources/Generated/Swift"
        )
    ],
    swiftLanguageVersions: [.v5]
)
