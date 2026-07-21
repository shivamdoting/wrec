// swift-tools-version: 6.0
// wrec's native macOS shell. One tiny binary, zero dependencies, talks to the
// Rust daemon over a Unix socket. Built with whole-module optimization in
// release; no Xcode project — `swift build` is the whole story.
import PackageDescription

let package = Package(
    name: "wrec-mac",
    platforms: [.macOS(.v15)],
    targets: [
        .executableTarget(
            name: "wrec-app",
            path: "Sources/WrecApp",
            resources: [
                .copy("Resources/DepartureMono-Regular.otf"),
                .copy("Resources/SKILL.md"),
            ]
        ),
        .testTarget(name: "WrecAppTests", dependencies: ["wrec-app"]),
    ]
)
