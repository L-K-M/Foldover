// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "FoldoverSystem",
    platforms: [.macOS("27.0")],
    products: [.library(name: "FoldoverSystem", targets: ["FoldoverSystem"])],
    targets: [
        .target(
            name: "FoldoverSystem",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "FoldoverSystemTests",
            dependencies: ["FoldoverSystem"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
