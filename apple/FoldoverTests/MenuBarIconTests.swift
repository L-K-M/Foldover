import AppKit
import Testing

@MainActor
@Test(arguments: ["MenuBarArrow", "MenuBarArrowAttention", "MenuBarArrowWorking", "MenuBarArrowPaused"])
func menuBarArrowsLoadAsTemplateImages(_ name: String) throws {
    // Exercise the compiled assets, including the SVG decoder and template metadata.
    let image = try #require(NSImage(named: NSImage.Name(name)))
    #expect(image.isTemplate)
    #expect(image.size == NSSize(width: 20, height: 18))

    let bitmap = try #require(image.cgImage(forProposedRect: nil, context: nil, hints: nil))
    #expect(bitmap.width > 0)
    #expect(bitmap.height > 0)
}
