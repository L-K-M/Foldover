// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "FoldoverSystem",
    platforms: [.macOS("27.0")],
    products: [.library(name: "FoldoverSystem", targets: ["FoldoverSystem"])],
    dependencies: [
        // Updates (PLAN.md §9); bump with the appcast tools in scripts/appcast.sh.
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.0"),
    ],
    targets: [
        .target(
            name: "FoldoverSystem",
            dependencies: [.product(name: "Sparkle", package: "Sparkle")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
