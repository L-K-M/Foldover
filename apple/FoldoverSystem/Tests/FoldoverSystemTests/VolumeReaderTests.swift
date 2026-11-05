import Foundation
import Testing
@testable import FoldoverSystem

struct VolumeReaderTests {
    @Test func readsTheVolumeAFolderIsOn() async throws {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let reading = try #require(await VolumeReader.read(home))
        #expect(!reading.name.isEmpty)
        #expect(reading.totalBytes > 0)
        #expect(reading.availableBytes <= reading.totalBytes)
    }

    @Test func aMissingFolderHasNoVolume() async {
        let gone = FileManager.default.temporaryDirectory.appending(path: "foldover-\(UUID().uuidString)")
        #expect(await VolumeReader.read(gone) == nil)
    }

    /// A second APFS volume reports 0 bytes free for important use while
    /// it has gigabytes free (seen on macOS 27 for a data volume and for
    /// Recovery): the plain figure must stand, not "Zero KB free".
    @Test func aVolumeThatCannotWeighPurgeableSpaceKeepsItsPlainFigure() {
        #expect(VolumeReader.available(important: 0, plain: 18_722_811_904) == 18_722_811_904)
        #expect(VolumeReader.available(important: nil, plain: 5) == 5)
        #expect(VolumeReader.available(important: 238_570_598_256, plain: 222_482_227_200) == 238_570_598_256)
        #expect(VolumeReader.available(important: nil, plain: nil) == nil)
        // A 0 for important use with no plain figure says nothing: unknown.
        #expect(VolumeReader.available(important: 0, plain: nil) == nil)
        #expect(VolumeReader.available(important: -1, plain: nil) == nil)
        // A full disk that says so reads as full.
        #expect(VolumeReader.available(important: 0, plain: 0) == 0)
    }

    @Test func freeSpaceNeverExceedsTheTotal() {
        #expect(VolumeReading(name: "Disk", totalBytes: 10, availableBytes: 12).availableBytes == 10)
    }
}
