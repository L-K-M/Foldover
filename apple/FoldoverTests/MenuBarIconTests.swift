import AppKit
import Testing
@testable import Foldover

@MainActor
@Test(arguments: ["MenuBarArrow", "MenuBarArrowAttention", "MenuBarArrowWorking", "MenuBarArrowPaused",
                  "MenuBarArrowWorkingSoft", "MenuBarArrowWorkingDim"])
func menuBarArrowsLoadWithTheirIntendedColorMode(_ name: String) throws {
    // Exercise the compiled assets, including the SVG decoder and template metadata.
    let image = try #require(NSImage(named: NSImage.Name(name)))
    #expect(image.isTemplate == (name != MenuBarStatus.attention.rawValue))
    #expect(image.size == NSSize(width: 20, height: 18))

    let bitmap = try #require(image.cgImage(forProposedRect: nil, context: nil, hints: nil))
    #expect(bitmap.width > 0)
    #expect(bitmap.height > 0)
}

@MainActor
private func arrowPixel(_ name: String, x: Int = 10, y: Int = 9) throws -> NSColor {
    let image = try #require(NSImage(named: NSImage.Name(name)))
    let bitmap = try #require(image.cgImage(forProposedRect: nil, context: nil, hints: nil))
    let pixels = NSBitmapImageRep(cgImage: bitmap)
    let pixelX = Int((CGFloat(x) + 0.5) * CGFloat(bitmap.width) / image.size.width)
    let pixelY = Int((CGFloat(y) + 0.5) * CGFloat(bitmap.height) / image.size.height)
    return try #require(pixels.colorAt(x: pixelX, y: pixelY)?.usingColorSpace(.deviceRGB))
}

@MainActor
@Test
func theAttentionArrowIsRedWithAWhiteExclamationMark() throws {
    let red = try arrowPixel(MenuBarStatus.attention.rawValue)
    #expect(red.redComponent > red.greenComponent + 0.5)
    #expect(red.redComponent > red.blueComponent + 0.5)

    let white = try arrowPixel(MenuBarStatus.attention.rawValue, x: 16, y: 13)
    #expect(white.redComponent > 0.9)
    #expect(white.greenComponent > 0.9)
    #expect(white.blueComponent > 0.9)
}

@MainActor
@Test
func theWorkingFramesPulseWithoutDisappearing() throws {
    let full = try arrowPixel(MenuBarStatus.working.rawValue)
    let soft = try arrowPixel("MenuBarArrowWorkingSoft")
    let dim = try arrowPixel("MenuBarArrowWorkingDim")
    #expect(full.alphaComponent > soft.alphaComponent)
    #expect(soft.alphaComponent > dim.alphaComponent)
    #expect(dim.alphaComponent > 0.7)
}
