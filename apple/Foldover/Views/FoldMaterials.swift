// The fold's materials: paper, the crease, the two folders' inks, the
// blind impression a copy lands in, and the highlighter that marks the
// selected line. Each folder keeps one ink everywhere, so a line's color
// always says whose version it is; position and a written verb say the
// same thing for anyone who cannot tell the inks apart.

import AppKit
import SwiftUI

enum FoldPalette {
    /// Folder A writes in Prussian blue, Folder B in vermilion: a pair
    /// that stays apart for red-green color blindness and in grayscale.
    static func ink(_ side: FolderSide) -> Color {
        switch side {
        case .a: dynamic(light: 0x23408E, dark: 0x93AAF5)
        case .b: dynamic(light: 0xB0441E, dark: 0xF3A07F)
        }
    }

    /// Warm, uncoated stock.
    static let paper = dynamic(light: 0xF7F2E8, dark: 0x2B2824)
    /// The back of the sheet, seen once the leaf turns past upright.
    static let paperBack = dynamic(light: 0xECE5D6, dark: 0x24211D)
    /// What a lifted half uncovers.
    static let desk = dynamic(light: 0xDDD6C9, dark: 0x191714)
    /// Pencil: headings, sizes and verbs.
    static let pencil = dynamic(light: 0x6F675C, dark: 0xA39A8C)
    /// The impression's fill: paper pressed a shade darker, still legible.
    static let impression = dynamic(light: 0x9A9183, dark: 0x847B6E)
    /// The pin through a conflict.
    static let pin = dynamic(light: 0xD98A0B, dark: 0xF2A93B)
    static let failure = Color(nsColor: .systemRed)

    private static func dynamic(light: UInt32, dark: UInt32) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            return NSColor(rgb: isDark ? dark : light)
        })
    }
}

private extension NSColor {
    convenience init(rgb: UInt32) {
        self.init(srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255, green: CGFloat((rgb >> 8) & 0xFF) / 255,
                  blue: CGFloat(rgb & 0xFF) / 255, alpha: 1)
    }
}

enum FoldType {
    /// File names in a half.
    static let name = Font.system(size: 13, weight: .medium)
    /// Verbs and sizes at a half's outer edge, in small caps.
    static let meta = Font.system(size: 10.5, weight: .medium).smallCaps()
    /// Folder headings: the press's serif, set small.
    static let heading = Font.system(size: 12, design: .serif).italic()
    /// The imprint's figures.
    static let figure = Font.system(size: 34, weight: .regular, design: .serif).monospacedDigit()
    static let imprintTitle = Font.system(size: 13, weight: .semibold, design: .serif).smallCaps()
}

/// Paper with a faint, fixed grain: the same speckles at every size, so
/// a resize never makes the sheet shimmer.
struct Paper: View {
    var tone: Color = FoldPalette.paper

    var body: some View {
        Rectangle()
            .fill(tone)
            .overlay {
                Canvas { context, size in
                    var random = SplitMix(seed: 0xF01D)
                    let fibres = Int(size.width * size.height / 140)
                    for _ in 0..<fibres {
                        let x = random.unit() * size.width
                        let y = random.unit() * size.height
                        let length = 0.6 + random.unit() * 2.2
                        let dark = random.unit() < 0.5
                        let rect = CGRect(x: x, y: y, width: length, height: 0.6)
                        context.fill(Path(rect), with: .color(dark ? .black.opacity(0.035) : .white.opacity(0.05)))
                    }
                }
                .allowsHitTesting(false)
            }
            .accessibilityHidden(true)
    }
}

/// The fold line down the middle of the sheet: a valley, darker while
/// the sheet is folded. Its lit side is faint on dark paper, where a
/// bright edge would glow.
struct Crease: View {
    /// 0 flat … 1 folded.
    var depth: Double = 0
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let shade = scheme == .dark ? 0.25 : 0.05
        let lit = scheme == .dark ? 0.05 : 0.35
        HStack(spacing: 0) {
            LinearGradient(colors: [.clear, .black.opacity(shade + 0.08 * depth)],
                           startPoint: .leading, endPoint: .trailing)
            Rectangle().fill(.black.opacity(0.10 + 0.10 * depth)).frame(width: 1)
            LinearGradient(colors: [.white.opacity(lit), .clear], startPoint: .leading, endPoint: .trailing)
        }
        .frame(width: 22)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

extension View {
    /// Blind-embossed: no ink, just paper pressed in, lit from above.
    /// Increase Contrast prints it in pencil instead.
    func impression(_ contrast: ColorSchemeContrast) -> some View {
        modifier(Impression(contrast: contrast))
    }
}

private struct Impression: ViewModifier {
    let contrast: ColorSchemeContrast
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        if contrast == .increased {
            content.foregroundStyle(FoldPalette.pencil)
        } else {
            // Light catches the lower lip of a pressed letter and shades
            // its upper edge; on dark paper the catch light is faint.
            content
                .foregroundStyle(FoldPalette.impression)
                .shadow(color: .white.opacity(scheme == .dark ? 0.07 : 0.75), radius: 0, x: 0, y: 1)
                .shadow(color: .black.opacity(scheme == .dark ? 0.55 : 0.22), radius: 0, x: 0, y: -0.5)
        }
    }
}

extension View {
    /// Letterpress: set a hair below the paper, lit from above.
    func letterpress() -> some View {
        modifier(Letterpress())
    }
}

private struct Letterpress: ViewModifier {
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        content.shadow(color: scheme == .dark ? .black.opacity(0.5) : .white.opacity(0.6), radius: 0, x: 0, y: 1)
    }
}

/// A highlighter stroke behind the selected line, in the accent color
/// while the sheet has the keys, gray while it does not, as selection
/// reads everywhere on the Mac.
struct Highlighter: View {
    var focused: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 4)
            .fill(focused ? Color.accentColor.opacity(0.24) : Color.gray.opacity(0.18))
            .rotationEffect(.degrees(-0.35))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .accessibilityHidden(true)
    }
}

/// The turned-down corner a person drags to fold the half it sits on:
/// the flap, showing the paper's back, and the corner it uncovered.
struct DogEar: Shape {
    enum Part {
        case flap
        case cut
    }

    let side: FolderSide
    let part: Part

    func path(in rect: CGRect) -> Path {
        // Corners of the square, starting top-left, clockwise.
        let tl = CGPoint(x: rect.minX, y: rect.minY), tr = CGPoint(x: rect.maxX, y: rect.minY)
        let br = CGPoint(x: rect.maxX, y: rect.maxY), bl = CGPoint(x: rect.minX, y: rect.maxY)
        let points: [CGPoint] = switch (side, part) {
        case (.a, .cut): [tl, tr, bl]
        case (.a, .flap): [tr, br, bl]
        case (.b, .cut): [tr, br, tl]
        case (.b, .flap): [tl, br, bl]
        }
        var path = Path()
        path.addLines(points)
        path.closeSubpath()
        return path
    }
}

/// The pin through a conflict's line: nothing on it moves until a person
/// chooses a version.
struct Pin: View {
    var body: some View {
        Circle()
            .fill(FoldPalette.pin.gradient)
            .overlay(Circle().fill(.white.opacity(0.55)).frame(width: 3, height: 3).offset(x: -1.5, y: -1.5))
            .frame(width: 10, height: 10)
            .shadow(color: .black.opacity(0.35), radius: 1, x: 0.5, y: 1)
    }
}

/// A seeded generator: the grain is drawn the same every time.
private struct SplitMix {
    private var state: UInt64

    init(seed: UInt64) { state = seed }

    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }

    /// A value in 0..<1.
    mutating func unit() -> Double { Double(next() >> 11) / Double(1 << 53) }
}
