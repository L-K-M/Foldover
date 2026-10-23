// What the key window offers the menu bar's commands (PLAN.md §4.5).

import SwiftUI

extension FocusedValues {
    /// The main window's model while that window is key, so the Pair,
    /// View and Edit commands act on its selection and turn off while
    /// another window is in front.
    @Entry var mainWindow: MainViewModel?
}
