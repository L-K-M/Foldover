// The view model is the only source of the strings the views render;
// the version text must carry the store's version.

import Testing
@testable import Foldover

@MainActor
@Test
func textsIncludeTheCoreVersion() {
    let store = AppInfoStore()
    let viewModel = MainViewModel(appInfo: store)

    #expect(viewModel.coreVersionText == "Core \(store.coreVersion)")
    #expect(!viewModel.emptyState.isEmpty)
    #expect(!viewModel.statusLine.isEmpty)
}
