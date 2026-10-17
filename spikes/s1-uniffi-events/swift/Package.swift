// swift-tools-version: 6.0

// The S1 spike's SwiftPM package: `S1BenchCoreFFI` is the XCFramework
// run.sh builds over the Rust staticlib, `S1BenchCore` is the UniFFI
// bindings generated into `S1BenchCore/Sources/` and `Headers/` (both
// gitignored; run.sh regenerates them), and `Bench` is the benchmark
// executable. Everything under `S1BenchCore/` is generated except
// this manifest.

import PackageDescription

let package = Package(
    name: "S1Bench",
    platforms: [.macOS("27.0")],
    products: [
        .executable(name: "Bench", targets: ["Bench"])
    ],
    targets: [
        .binaryTarget(
            name: "S1BenchCoreFFI",
            path: "S1BenchCore/S1BenchCoreFFI.xcframework"
        ),
        .target(
            name: "S1BenchCore",
            dependencies: ["S1BenchCoreFFI"],
            path: "S1BenchCore/Sources/S1BenchCore",
            // The generated code under the same strict-concurrency
            // rule the app builds with. `-warnings-as-errors` is left
            // to Bench: generated files can't be edited anyway.
            swiftSettings: [
                .swiftLanguageMode(.v6),
                .unsafeFlags(["-strict-concurrency=complete"]),
            ],
            linkerSettings: [
                // rustc --print native-static-libs: the Rust staticlib
                // needs libiconv at link time.
                .linkedLibrary("iconv")
            ]
        ),
        .executableTarget(
            name: "Bench",
            dependencies: ["S1BenchCore"],
            path: "Sources/Bench",
            // The strict build S1's question is about: Swift 6
            // language mode, complete strict concurrency, warnings
            // as errors. unsafeFlags is allowed in a root package.
            swiftSettings: [
                .swiftLanguageMode(.v6),
                .unsafeFlags(["-warnings-as-errors", "-strict-concurrency=complete"]),
            ]
        ),
        .testTarget(
            name: "BenchTests",
            dependencies: ["Bench"]
        ),
    ]
)
