import SwiftUI

@MainActor
struct MenuBarScene: Scene {
    @State private var animation: MenuBarAnimation
    let viewModel: MainViewModel

    init(viewModel: MainViewModel) {
        self.viewModel = viewModel
        _animation = State(initialValue: MenuBarAnimation(viewModel: viewModel))
    }

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
    }
}
