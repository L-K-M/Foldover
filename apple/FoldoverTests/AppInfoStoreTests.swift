// The P0 FFI contract, seen from the app side: the UniFFI surface
// answers one call, a semver-shaped workspace version string.

import Testing
@testable import Foldover

@Test
func coreVersionIsNonEmpty() {
    #expect(!AppInfoStore().coreVersion.isEmpty)
}

@Test
func coreVersionLooksLikeXYZ() {
    let parts = AppInfoStore().coreVersion.split(separator: ".")

    #expect(parts.count == 3)
    #expect(parts.allSatisfy { part in
        !part.isEmpty && part.allSatisfy { $0.isNumber }
    })
}
