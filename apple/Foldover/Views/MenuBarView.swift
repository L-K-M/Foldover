// The menu-bar surface (PLAN.md §3.8): every pair's status with a way to
// sync it, Sync All, automatic-sync controls, the main window, Settings
// and Quit.

import AppKit
import SwiftUI

struct MenuBarView: View {
    @Environment(\.openWindow) private var openWindow
    @ScaledMetric(relativeTo: .title2) private var headerIconSize: CGFloat = 24
    let viewModel: MainViewModel

    /// Rough height of one pair row at default text size, with slack
    /// for a second caption line. Wrapped errors grow rows further, so
    /// the threshold errs toward scrolling early.
    @ScaledMetric(relativeTo: .body) private var estimatedRowHeight: CGFloat = 44
    /// Pair-list height past which rows scroll instead of pushing the
    /// controls below off screen.
    private static let maxPairListHeight: CGFloat = 280

    var body: some View {
        let rows = viewModel.pairRows
        VStack(alignment: .leading, spacing: 8) {
            header

            if !rows.isEmpty {
                Divider()
                pairList(rows)
                Divider()
                Button { Task { await viewModel.syncAll() } } label: { rowLabel("Sync All") }
                    .disabled(!viewModel.canSyncAll)
            }

            automaticSyncs

            if let error = viewModel.operationError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            Divider()

            Button { openMainWindow() } label: { rowLabel("Open Foldover") }
            SettingsLink { rowLabel("Settings…") }
            Button { NSApplication.shared.terminate(nil) } label: { rowLabel("Quit Foldover") }
                .keyboardShortcut("q")
        }
        // Rows like a menu's: no bezels, spanning the width.
        .buttonStyle(.borderless)
        .padding(12)
        .frame(width: 300)
        // Long agent messages wrap; the window never grows unbounded.
        .frame(maxHeight: 560)
    }

    /// Every pair's row. Scrolls when the estimated height outruns the
    /// bound; the estimate is deliberately generous so tall rows (wrapped
    /// errors, large text) start scrolling before they can clip Quit.
    @ViewBuilder
    private func pairList(_ rows: [PairRow]) -> some View {
        let stack = VStack(alignment: .leading, spacing: 2) {
            ForEach(rows) { row in
                pairRow(row)
            }
        }
        if CGFloat(rows.count) * estimatedRowHeight > Self.maxPairListHeight {
            ScrollView { stack }
                .frame(maxHeight: Self.maxPairListHeight)
        } else {
            stack
        }
    }

    /// The menu bar symbol, the catalog and the background agent, with
    /// the way out of safe mode while syncing waits in it.
    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 10) {
                Image(viewModel.menuBarStatus.rawValue)
                    .resizable()
                    .scaledToFit()
                    .foregroundStyle(.tint)
                    .frame(width: headerIconSize, height: headerIconSize)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(viewModel.statusLine)
                        .font(.headline)
                    Text(viewModel.agentSummary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityAddTraits(.updatesFrequently)
                }
            }
            .accessibilityElement(children: .combine)
            // Outside the combined element, so VoiceOver reaches it.
            if viewModel.agentAction == .resumeSyncing {
                Button("Resume Syncing") { Task { await viewModel.perform(.resumeSyncing) } }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .padding(.leading, 34)
            }
            // A failed resume stays beside the button that retries it.
            if let error = viewModel.safeModeError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .padding(.leading, 34)
            }
        }
    }

    /// The pair's name and status open it in the main window; the
    /// trailing button syncs it from here.
    private func pairRow(_ row: PairRow) -> some View {
        HStack(spacing: 8) {
            Button { openMainWindow(selecting: row.id) } label: {
                HStack(spacing: 8) {
                    Image(systemName: row.symbol)
                        .foregroundStyle(.secondary)
                        .frame(width: 18)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(row.name)
                            .foregroundStyle(.primary)
                            .lineLimit(1)
                        Text(row.status)
                            .font(.caption)
                            .foregroundStyle(row.activity == .attention ? Color.orange : Color.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            // Plain keeps the name in the primary color, like a menu item.
            .buttonStyle(.plain)
            .help("Show \(row.name) in Foldover")

            if row.activity == .working {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Working")
            }

            Button("Sync \(row.name)", systemImage: "arrow.triangle.2.circlepath") {
                Task { await viewModel.syncPair(id: row.id) }
            }
            .labelStyle(.iconOnly)
            .disabled(!row.canSync)
            .help("Sync \(row.name)")
        }
    }

    /// Pause or resume automatic syncs, with what holds them.
    @ViewBuilder
    private var automaticSyncs: some View {
        if let pause = viewModel.pauseText {
            note(pause)
            Button { Task { await viewModel.resume() } } label: { rowLabel("Resume Automatic Syncs") }
        } else {
            if let hold = viewModel.holdText {
                note(hold)
            }
            Menu("Pause Automatic Syncs") {
                ForEach(PauseLength.allCases, id: \.self) { length in
                    Button(MainViewModel.title(length)) { Task { await viewModel.pause(length) } }
                }
            }
            .menuStyle(.button)
            .fixedSize()
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func rowLabel(_ title: LocalizedStringKey) -> some View {
        Text(title)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
    }

    /// Bring the main window forward, showing `pairID` when given. The
    /// extra does not activate the app by itself.
    private func openMainWindow(selecting pairID: String? = nil) {
        if let pairID { viewModel.selectedPairID = pairID }
        openWindow(id: "main")
        NSApplication.shared.activate()
    }
}
