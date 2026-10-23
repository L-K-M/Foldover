import SwiftUI

@MainActor
struct MenuBarScene: Scene {
    @State private var animation = MenuBarAnimation()
    @State private var motion = MotionStore()
    let viewModel: MainViewModel

    var body: some Scene {
        let status = viewModel.menuBarStatus
        MenuBarExtra {
            MenuBarView(viewModel: viewModel)
        } label: {
            // Status items snapshot images; view animations do not reach them.
            Label {
                Text(status.accessibilityLabel)
            } icon: {
                Image(animation.imageName(for: status))
            }
        }
        .menuBarExtraStyle(.window)
        // Scene lifetime keeps the pulse running with every window closed.
        .onChange(of: status, initial: true) { _, status in
            animation.update(status: status, motion: motion.preference)
        }
        .onChange(of: motion.preference) { _, preference in
            animation.update(status: status, motion: preference)
        }
    }
}
