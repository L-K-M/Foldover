// swift-tools-version: 6.0

// The generated core bindings as a SwiftPM package (PLAN.md §4.5).
// `FoldoverCoreFFI` is the XCFramework `cargo xtask ffi` builds on
// macOS; `FoldoverCore` is the generated Swift surface on top of it.
// Everything in `Sources/` and `Headers/` is generated — only this
// manifest is handwritten.

import PackageDescription

let package = Package(
    name: "FoldoverCore",
    platforms: [.macOS("26.0")],
    products: [
        .library(name: "FoldoverCore", targets: ["FoldoverCore"])
    ],
    targets: [
        .binaryTarget(
            name: "FoldoverCoreFFI",
            path: "FoldoverCoreFFI.xcframework"
        ),
        .target(
            name: "FoldoverCore",
            dependencies: ["FoldoverCoreFFI"],
            path: "Sources/FoldoverCore",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
