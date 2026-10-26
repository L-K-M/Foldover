// swift-tools-version: 6.0

// The generated core bindings as a SwiftPM package (PLAN.md §4.5).
// `FoldoverCoreFFI` is the XCFramework `cargo xtask ffi` builds on
// macOS; `FoldoverCore` is the generated Swift surface on top of it.
// Everything in `Sources/` and `Headers/` is generated — only this
// manifest is handwritten.
//
// The generated files contain code from UniFFI's Swift and C templates
// (uniffi_bindgen 0.32.2, https://github.com/mozilla/uniffi-rs), which
// is under the MPL-2.0, not the repository's Unlicense. The app's
// Third-Party-Notices.txt carries that license and the source location.

import PackageDescription

let package = Package(
    name: "FoldoverCore",
    platforms: [.macOS("27.0")],
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
            swiftSettings: [.swiftLanguageMode(.v6)],
            linkerSettings: [.linkedFramework("Security"), .linkedFramework("CoreFoundation")]
        ),
    ]
)
