// The fold's materials, after the app icon: origami paper (kami), ivory
// on its face and cerulean on its back, laid on a navy field. The sheet
// is the paper's face; the leaf shows the back once it turns, so a
// folded sheet looks like the icon. Each folder keeps one ink
// everywhere, so a line's color always says whose version it is;
// position and a written verb say the same thing for anyone who cannot
// tell the inks apart.
//
//   navy field  ┌──── ivory face ────┐
//               │ A ink  │  B ink    │   the back, once turned:
//               └────────┴───────────┘   ◤ cerulean, two facets ◢

import AppKit
import SwiftUI

enum FoldPalette {
    /// Folder A writes in navy, Folder B in cerulean: far apart in
    /// lightness, so they hold in grayscale and for every kind of color
    /// vision. On dark paper navy gives way to the icon's ivory.
    static func ink(_ side: FolderSide) -> Color {
        switch side {
        case .a: dynamic(light: 0x123E80, dark: 0xEDE3D3)
        case .b: dynamic(light: 0x0A66BD, dark: 0x5BB8FF)
        }
    }

    /// The paper's face: the icon's ivory, or midnight paper in Dark Mode.
    static let paper = dynamic(light: 0xF4ECE1, dark: 0x1E2B42)
    /// The paper's back, cerulean like the icon's arrowhead: its lit and
    /// its shaded facet. White type on it stays legible on either.
    static let kamiLit = dynamic(light: 0x1E9BF0, dark: 0x1689DA)
    static let kamiShade = dynamic(light: 0x0659B2, dark: 0x054C99)
    /// The navy field under the sheet: where the light falls, and the
    /// vignette it fades into.
    static let desk = dynamic(light: 0x0B5AA6, dark: 0x052A55)
    static let deskShade = dynamic(light: 0x002457, dark: 0x000A1A)
    /// Headings, sizes and verbs.
    static let slate = dynamic(light: 0x56647A, dark: 0x93A3BC)
    /// The pin through a conflict.
    static let pin = Color(nsColor: .systemOrange)
    static let failure = Color(nsColor: .systemRed)

    /// The back's two facets as one fill, for small pieces of it.
    static var kami: LinearGradient {
        LinearGradient(colors: [kamiLit, kamiShade], startPoint: .topLeading, endPoint: .bottomTrailing)
    }

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
    /// Verbs and sizes at a half's outer edge.
    static let meta = Font.system(size: 11).monospacedDigit()
    /// Small labels above a value, set in capitals: "FOLDER A".
    static let eyebrow = Font.system(size: 9.5, weight: .semibold)
    static let eyebrowTracking: CGFloat = 0.8
    /// Folder headings in the list.
    static let heading = Font.system(size: 11, weight: .semibold)
    /// Large counts and the run's percentage.
    static let figure = Font.system(size: 36, weight: .semibold).monospacedDigit()
}

/// A fine, fixed grain: the same fibres at every size, so a resize
/// never makes the paper shimmer.
private struct Grain: View {
    var body: some View {
        Canvas { context, size in
            var random = SplitMix(seed: 0xF01D)
            let fibres = Int(size.width * size.height / 160)
            for _ in 0..<fibres {
                let x = random.unit() * size.width
                let y = random.unit() * size.height
                let length = 0.5 + random.unit() * 1.6
                let dark = random.unit() < 0.5
                let rect = CGRect(x: x, y: y, width: length, height: 0.5)
                context.fill(Path(rect), with: .color(dark ? .black.opacity(0.04) : .white.opacity(0.05)))
            }
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// The paper's face.
struct Paper: View {
    var body: some View {
        Rectangle()
            .fill(FoldPalette.paper)
            .overlay { Grain() }
            .accessibilityHidden(true)
    }
}

/// The paper's back: cerulean, lit from the top left and split along a
/// diagonal crease into a brighter and a deeper facet, as the icon's
/// arrowhead is.
struct Kami: View {
    var body: some View {
        Rectangle()
            .fill(FoldPalette.kami)
            .overlay { Facet().fill(.white.opacity(0.08)) }
            .overlay { Grain() }
            .accessibilityHidden(true)
    }

    /// The lit facet: the triangle above the diagonal from the top
    /// right corner to the bottom left one.
    private struct Facet: Shape {
        func path(in rect: CGRect) -> Path {
            var path = Path()
            path.addLines([CGPoint(x: rect.minX, y: rect.minY), CGPoint(x: rect.maxX, y: rect.minY),
                           CGPoint(x: rect.minX, y: rect.maxY)])
            path.closeSubpath()
            return path
        }
    }
}

/// The navy field the sheet lies on: light pools at the top left and
/// falls away to the far corner, as on the icon.
struct Desk: View {
    var body: some View {
        EllipticalGradient(colors: [FoldPalette.desk, FoldPalette.deskShade], center: .topLeading,
                           startRadiusFraction: 0, endRadiusFraction: 1.3)
            .overlay { Grain() }
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
        let shade = scheme == .dark ? 0.28 : 0.06
        let lit = scheme == .dark ? 0.04 : 0.5
        HStack(spacing: 0) {
            LinearGradient(colors: [.clear, .black.opacity(shade + 0.1 * depth)],
                           startPoint: .leading, endPoint: .trailing)
            Rectangle().fill(.black.opacity(0.12 + 0.1 * depth)).frame(width: 1)
            LinearGradient(colors: [.white.opacity(lit), .clear], startPoint: .leading, endPoint: .trailing)
        }
        .frame(width: 16)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// A flat tint behind the selected line: cerulean while the sheet has
/// the keys, gray while it does not, as selection reads everywhere on
/// the Mac.
struct SelectionTint: View {
    var focused: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 6, style: .continuous)
            .fill(focused ? FoldPalette.kamiLit.opacity(0.18) : Color.gray.opacity(0.16))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .accessibilityHidden(true)
    }
}

/// The turned-down corner a person drags to fold the half it sits on:
/// the flap, showing the paper's cerulean back, and the corner it
/// uncovered.
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
            .fill(FoldPalette.pin)
            .overlay(Circle().strokeBorder(FoldPalette.paper, lineWidth: 1.5))
            .frame(width: 11, height: 11)
            .shadow(color: .black.opacity(0.25), radius: 1, x: 0, y: 0.5)
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
