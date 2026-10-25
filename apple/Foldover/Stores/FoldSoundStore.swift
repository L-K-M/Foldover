// The fold's paper sounds, synthesized by tools/sounds/make_fold_sounds.py.
// They answer what a person does in the main window, never a background
// sync, and stay quiet when macOS's "Play user interface sound effects"
// is off or the Settings switch is.

import AppKit

/// A sound the sheet makes.
enum FoldSound: String, CaseIterable, Sendable {
    /// The leaf swinging through the air.
    case swish = "FoldSwish"
    /// The leaf landing flat: the sync starts.
    case press = "FoldPress"
    /// The leaf lifting off when the run ends.
    case peel = "FoldPeel"
    /// A line turned another way.
    case tick = "FoldTick"
}

@MainActor
final class FoldSoundStore {
    /// The Settings switch; sounds play unless it is turned off.
    static let preferenceKey = "paperSounds"
    /// macOS's "Play user interface sound effects", a global default.
    private static let systemKey = "com.apple.sound.uiaudio.enabled"
    private let defaults: UserDefaults
    /// Loaded on first use; replaying one restarts it.
    private var sounds: [FoldSound: NSSound] = [:]

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var isEnabled: Bool {
        get { defaults.object(forKey: Self.preferenceKey) as? Bool ?? true }
        set { defaults.set(newValue, forKey: Self.preferenceKey) }
    }

    func play(_ sound: FoldSound) {
        let systemAllows = (defaults.object(forKey: Self.systemKey) as? NSNumber)?.boolValue ?? true
        guard isEnabled, systemAllows else { return }
        guard let player = sounds[sound] ?? NSSound(named: NSSound.Name(sound.rawValue)) else { return }
        sounds[sound] = player
        player.stop()
        player.play()
    }
}
