import AppKit
import SwiftUI
import Testing
@testable import Foldover

@MainActor
struct FoldDiskViewTests {
    @Test func capacityIsAHollowRingRatherThanAProgressBar() throws {
        let pixels = try renderDisk(available: 50, toCopy: 25)
        let centerY = pixels.pixelsHigh / 2

        // The compact ring sits beside the words, with an unpainted hole.
        #expect(try opacity(pixels, x: 20, y: centerY) == 0)
        #expect(try opacity(pixels, x: 32, y: centerY - 12) > 0.85)
        #expect(try opacity(pixels, x: 32, y: centerY + 12) > 0.85)

        // Half is stored, a quarter is arriving, and a quarter stays free.
        // Sort the left samples so bitmap row order does not matter.
        let left = try [opacity(pixels, x: 8, y: centerY - 12),
                        opacity(pixels, x: 8, y: centerY + 12)].sorted()
        #expect(left[0] > 0.15 && left[0] < 0.3)
        #expect(left[1] > 0.5 && left[1] < 0.7)
    }

    @Test(arguments: [UInt64(0), 100])
    func emptyAndFullDisksKeepTheRingHollow(_ available: UInt64) throws {
        let pixels = try renderDisk(available: available, toCopy: nil)
        let centerY = pixels.pixelsHigh / 2
        #expect(try opacity(pixels, x: 20, y: centerY) == 0)
        let rim = try opacity(pixels, x: 32, y: centerY - 12)
        if available == 0 {
            #expect(rim > 0.85)
        } else {
            #expect(rim > 0.15 && rim < 0.3)
        }
    }

    private func renderDisk(available: UInt64, toCopy: UInt64?) throws -> NSBitmapImageRep {
        let disk = FoldDisk(volume: FolderVolume(name: "Backup", totalBytes: 100, availableBytes: available),
                            toCopy: toCopy)
        let renderer = ImageRenderer(content: FoldDiskView(disk: disk).fixedSize())
        renderer.scale = 1
        return NSBitmapImageRep(cgImage: try #require(renderer.cgImage))
    }

    private func opacity(_ pixels: NSBitmapImageRep, x: Int, y: Int) throws -> CGFloat {
        try #require(pixels.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB)).alphaComponent
    }
}
