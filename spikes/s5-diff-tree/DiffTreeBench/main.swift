// `NSApplication` by hand — no `@main`, because the delegate must
// outlive this scope.
//
// Adapted from L-K-M/Sweetbriar (main.swift).

import AppKit

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
