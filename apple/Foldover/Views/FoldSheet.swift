// The selected pair's sheet (PLAN.md §3.8): its plan printed on the two
// pages of a creased sheet, folded to sync. The gutter between the pages
// holds only the crease; marks hang in the pages' outer margins.
//
//   ┌──────────────── paper ──────────────────────┐
//   │◤ Folder A            ┆   Folder B          ◥│  dog-ears: drag to fold
//   │ ▢ 2026/Trip/day2     ┆   2026/Trip/day2  3  │
//   │   IMG_2041.RAW       ┆   IMG_2041.RAW copy →│  ink ┆ arriving
//   │ ⚑ header.psd         ┆   header.psd   both ⚑│  a pin holds conflicts
//   └──────────────────────────────────────────────┘
//    5 to copy (120 MB), 2 to archive …   [Sync]   ← the desk strip
//
// Dragging a dog-ear swings that half over the crease. Past upright it
// falls, presses, and the sync starts; short of upright it falls back
// and nothing happens. Whatever starts a run (⌘S, the menu bar, a
// schedule), the leaf folds while it presses and opens on an imprint of
// the run's own counts when it ends. The scene follows the store's
// state; the leaf's swing is this view's own presentation.

import AppKit
import SwiftUI

/// What the sheet shows, from the pair's state.
private enum FoldScene: Equatable {
    /// A plan a person can review, steer and fold.
    case plan
    /// A preview, or a choice's re-preview, is comparing the folders.
    case comparing
    /// A run is pressing.
    case pressing
    /// An undo is lifting the last run.
    case lifting
    /// A run or an undo finished: its imprint.
    case imprint
    /// Anything else: no preview yet, out of date, stopped, waiting.
    case message

    var allowsInspection: Bool { self == .plan || self == .imprint }
}

/// Sounds accompany what a person did here; a fold that starts on its
/// own (⌘S from the menu, a schedule) moves without them.
private enum FoldVoice {
    case audible
    case silent
}

struct FoldSheet: View {
    @Bindable var viewModel: MainViewModel
    let sync: PairSyncViewModel
    let pair: SyncPair
    /// The listed changes the search leaves.
    let rows: [ChangeRow]
    let focus: FocusState<MainPane?>.Binding

    @Environment(\.accessibilityReduceMotion) private var systemReducesMotion
    @Environment(\.foldMotionPolicy) private var motionPolicy
    @Environment(\.foldTimeScale) private var timeScale
    @Environment(\.colorScheme) private var colorScheme

    /// How far the leaf has swung: 0 open … 1 folded flat. Animations
    /// set it to their end at once; drawing reads the swing in between
    /// through `SwingProgress`.
    @State private var swing: Double = 0
    @State private var leafSide: FolderSide = .a
    /// The leaf is down, or on its way, for a run; it opens when the run ends.
    @State private var leafDown = false
    /// The leaf has landed and pressed: the run's progress shows on it.
    @State private var landed = false
    /// The leaf is still swinging back open: its swing is already 0,
    /// its drawing not yet.
    @State private var opening = false
    /// Bumped by every fall and opening, so a superseded animation's
    /// completion does nothing.
    @State private var motion = 0
    @State private var voice = FoldVoice.silent
    @State private var drag: LeafDrag?
    @GestureState private var pointerDown = false
    /// The plan the sheet last showed: its token binds a fold, and its
    /// lines pick the half that folds on its own.
    @State private var plan: FoldPlan?
    /// The lines the plan last showed, search applied: a re-check after a
    /// choice and a run keep them on screen, and the leaf carries them.
    @State private var shown: [FoldLine] = []
    @State private var paperSize: CGSize = .zero
    @State private var scrollMark = ScrollMark()
    @State private var staged: StagedTurn?
    @State private var approval: ApprovalRequest?
    @State private var hoveredEar: FolderSide?
    /// Haptic and motion triggers.
    @State private var tips = 0
    @State private var presses = 0
    @State private var bump = false
    /// Shakes per line: each line keeps its own count, so shaking one
    /// never replays another's.
    @State private var shakes: [ChangeRow.ID: Int] = [:]
    /// A fresh plan printing onto a blank sheet, by its token: its first
    /// lines spread in one after another.
    @State private var printing: String?

    private static let headerHeight: CGFloat = 50
    private static let earSpring = Animation.spring(duration: 0.22, bounce: 0.3)
    /// The desk's words beside its buttons get at least this much room.
    private static let deskTextRoom: CGFloat = 200
    private static let listInset: CGFloat = 6
    /// The printed lines' stagger, and how many lines it reaches.
    private static let printStagger = 0.018
    private static let printedLines = 30
    /// Keys held with these are someone else's shortcut.
    private static let shortcutModifiers: EventModifiers = [.command, .option, .control, .shift]

    /// How the leaf comes back open.
    private enum Opening {
        /// Let go short of a fold, or no fold after all.
        case fallBack
        /// After a run, lifted off the pressed half.
        case peel
    }

    /// The fold's timings, in seconds at full speed: a full fall from
    /// upright, a fall back from upright, the shortest drop, and the
    /// peel's rise and drop.
    private static let fallTime = 0.28
    private static let fallBackTime = 0.26
    private static let shortestDrop = 0.12
    private static let peelLift = 0.2
    private static let peelDrop = 0.22

    /// A drag on a dog-ear, bound to the plan it began on: a new plan
    /// arriving mid-drag cancels it.
    private struct LeafDrag {
        let side: FolderSide
        let token: String?
        var cancelled = false
    }

    /// A turn ← or → staged on a line, waiting for Return.
    private struct StagedTurn: Equatable {
        let lineID: ChangeRow.ID
        let name: String
        let flow: FoldFlow
        let title: String
    }

    /// Where the list is scrolled, outside SwiftUI's state: it changes on
    /// every scroll tick, and only the leaf reads it, when it lifts.
    private final class ScrollMark {
        var offset: CGFloat = 0
    }

    var body: some View {
        let scene = self.scene
        let lines = lines(for: scene)
        // Grouped once per change, not on every frame of the swing.
        let entries = FoldEntry.entries(lines)
        // The keys live on the whole sheet, not the list, so Escape can
        // cancel a fold begun with nothing listed, and a re-check keeps them.
        ScrollViewReader { proxy in
            VStack(spacing: 10) {
                SwingProgress(swing) { progress in
                    paper(scene: scene, entries: entries, progress: progress)
                }
                // Pressed, the paper settles a hair into the field.
                .offset(y: bump ? 1 : 0)
                // The rising leaf passes in front of the desk's words.
                .zIndex(1)
                desk(scene: scene)
            }
            .padding(.horizontal, 14)
            .padding(.bottom, 12)
            .focusable()
            .focused(focus, equals: .changes)
            .focusEffectDisabled()
            .onKeyPress(keys: [.upArrow, .downArrow], phases: [.down, .repeat]) { press in
                move(press, in: entries, proxy: proxy)
            }
            .onKeyPress(keys: [.leftArrow, .rightArrow], phases: .down) { press in stage(press) }
            .onKeyPress(.return, phases: .down) { press in plain(press) ? runStaged() : .ignored }
            .onKeyPress(.escape, phases: .down) { press in plain(press) ? cancelStagedOrDrag() : .ignored }
            .onKeyPress(.space, phases: .down) { press in
                plain(press) ? quickLook(viewModel.selectedChange) : .ignored
            }
            .onKeyPress(keys: [.delete, .deleteForward], phases: .down) { press in
                plain(press) ? skipSelected() : .ignored
            }
            // Edit > Copy copies the selected change's path.
            .onCopyCommand {
                guard let path = viewModel.selectedChange?.path else { return [] }
                return [NSItemProvider(object: path as NSString)]
            }
        }
        .onChange(of: sync.foldPlan, initial: true) { _, new in
            guard let new else { return }
            // A plan onto a blank sheet prints; a re-check re-inks in place.
            if plan == nil, !reduceMotion { printPlan(new.token) }
            plan = new
            shown = rows.map(FoldLine.init)
        }
        .onChange(of: sync.foldPlan?.token) { _, token in
            staged = nil
            if let request = approval, request.reviewToken != token { cancelApproval() }
        }
        .onChange(of: rows, initial: true) { _, new in
            if scene == .plan { shown = new.map(FoldLine.init) }
            // A search that hides the staged line lets go of its turn.
            if let turn = staged, !new.contains(where: { $0.id == turn.lineID }) { staged = nil }
        }
        .onChange(of: viewModel.selectedChangeID) { _, id in
            if let turn = staged, turn.lineID != id { staged = nil }
        }
        .onChange(of: scene) { old, new in sceneChanged(from: old, to: new) }
        // A gesture the system cancels never reaches onEnded.
        .onChange(of: pointerDown) { _, down in
            guard !down else { return }
            Task { @MainActor in
                guard drag != nil else { return }
                drag = nil
                if scene == .plan, !leafDown, approval == nil { open() }
            }
        }
        // A run already pressing when the pair is shown: the leaf is down.
        .onAppear {
            guard scene == .pressing else { return }
            leafSide = FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? [])
            leafDown = true
            landed = true
            swing = 1
        }
        .sensoryFeedback(.alignment, trigger: tips)
        .sensoryFeedback(.levelChange, trigger: presses)
        .confirmationDialog("Approve Sync?", isPresented: Binding(
            get: { approval != nil },
            set: { if !$0 { cancelApproval() } }
        ), presenting: approval) { request in
            Button("Archive and Sync", role: .destructive) { approve(request) }
            Button("Cancel", role: .cancel) { cancelApproval() }
        } message: { request in
            Text(request.message)
        }
    }

    // MARK: State

    private var scene: FoldScene {
        switch sync.job {
        case .preview?: return .comparing
        case .run?: return .pressing
        case .undo?: return .lifting
        case nil: break
        }
        if sync.isResolving { return .comparing }
        if sync.foldPlan != nil { return .plan }
        if sync.imprint != nil { return .imprint }
        return .message
    }

    private func lines(for scene: FoldScene) -> [FoldLine] {
        switch scene {
        case .plan, .imprint: rows.map(FoldLine.init)
        case .comparing, .pressing: shown
        case .lifting, .message: []
        }
    }

    private var rowsByID: [ChangeRow.ID: ChangeRow] {
        Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    private var reduceMotion: Bool {
        motionPolicy == .reduced || (motionPolicy == .system && systemReducesMotion)
    }

    private var halfWidth: CGFloat { paperSize.width / 2 }
    /// A page: half the paper, less half the gutter.
    private var pageWidth: CGFloat { max(0, halfWidth - FoldMetrics.creaseWidth / 2) }

    /// The page a message is set on, rather than across the gutter.
    private var messagePage: FolderSide { FoldLeaf.messagePage(pair.settings.mode, missing: sync.missingSide) }

    /// A person may fold a plan with lines while the pair can sync, the
    /// leaf is not already down or waiting for approval, and no turn
    /// waits for Return.
    private var canFold: Bool {
        scene == .plan && !leafDown && approval == nil && sync.canSync
            && (plan?.changes ?? 0) > 0 && staged == nil
    }

    private var foldHint: String {
        if staged != nil { return String(localized: "Confirm or cancel the change you turned first.") }
        if (plan?.changes ?? 0) == 0 { return String(localized: "Nothing to sync. Preview to check again.") }
        if !sync.canSync { return String(localized: "Preview first, or wait until the pair is idle.") }
        return String(localized: "Drag toward the middle to fold the sheet and sync, or press ⌘S.")
    }

    /// The half lifted off the page as the leaf, at swing `progress`.
    private func lifted(_ side: FolderSide, _ progress: Double) -> Bool { progress > 0 && leafSide == side }

    /// How much of `side`'s print shows at swing `progress`: none on the
    /// page lifted off as the leaf, which carries its own, and less on
    /// the page the leaf closes over.
    private func pageInk(_ side: FolderSide, _ progress: Double) -> Double {
        lifted(side, progress) ? 0 : FoldMotion.landingInk(progress)
    }

    /// `content` set on `side`'s page alone, clear of the gutter.
    private func onPage<Content: View>(_ side: FolderSide, @ViewBuilder _ content: () -> Content) -> some View {
        content()
            .frame(width: pageWidth > 0 ? pageWidth : nil)
            .frame(maxWidth: .infinity, alignment: side == .a ? .leading : .trailing)
    }

    private func folder(_ side: FolderSide) -> SelectedFolder {
        side == .a ? pair.settings.left : pair.settings.right
    }

    /// No shortcut modifier held.
    private func plain(_ press: KeyPress) -> Bool {
        press.modifiers.isDisjoint(with: Self.shortcutModifiers)
    }

    // MARK: Paper

    private func paper(scene: FoldScene, entries: [FoldEntry], progress: Double) -> some View {
        VStack(spacing: 0) {
            header(progress)
            content(scene: scene, entries: entries, progress: progress)
        }
        // The page's shadow falls on the navy from the paper alone, not
        // from every line of text on it: a tight contact shadow and the
        // icon's deep cast, down and to the right, away from the light.
        // Pressed, the paper flattens onto the field and both tighten.
        .background {
            let dark = colorScheme == .dark
            halves(scene: scene, progress: progress)
                .compositingGroup()
                .shadow(color: FoldPalette.castShadow.opacity((dark ? 0.7 : 0.5) + (bump ? 0.12 : 0)),
                        radius: 1.5, x: 0, y: 1)
                .shadow(color: FoldPalette.castShadow.opacity(dark ? 0.65 : 0.45),
                        radius: bump ? 10 : 20, x: 3, y: bump ? 4 : 9)
                // In the paper, under the ink printed across it; there is
                // no fold where a half is missing.
                .overlay {
                    if sync.missingSide == nil { Crease(depth: progress) }
                }
        }
        .overlay(alignment: .topLeading) {
            if progress > 0, paperSize.width > 0 {
                FoldLeafView(side: leafSide, progress: progress, entries: entries, pageSize: paperSize,
                             scrollOffset: scrollMark.offset, headerHeight: Self.headerHeight,
                             listInset: Self.listInset, reduceMotion: reduceMotion) {
                    FoldFolderLabel(viewModel: viewModel, folder: folder(leafSide), side: leafSide)
                }
                .allowsHitTesting(false)
                .accessibilityHidden(true)
                .transition(.identity)
            }
        }
        .overlay(alignment: .topLeading) {
            if scene == .pressing, landed {
                pressReadout
                    .transition(.opacity)
            }
        }
        .overlay(alignment: .top) { dogEars(scene: scene, progress: progress) }
        .onGeometryChange(for: CGSize.self) { $0.size } action: { paperSize = $0 }
    }

    /// The two halves of the page: paper, the desk where the leaf lifted
    /// off, or a dashed outline where a folder is not connected.
    private func halves(scene: FoldScene, progress: Double) -> some View {
        HStack(spacing: 0) {
            half(.a, scene: scene, progress: progress)
            half(.b, scene: scene, progress: progress)
        }
        .animation(reduceMotion ? nil : Self.earSpring, value: hoveredEar)
    }

    @ViewBuilder
    private func half(_ side: FolderSide, scene: FoldScene, progress: Double) -> some View {
        if sync.missingSide == side {
            RoundedRectangle(cornerRadius: 4)
                .strokeBorder(.white.opacity(0.45), style: StrokeStyle(lineWidth: 1, dash: [6, 4]))
                .padding(3)
        } else if lifted(side, progress) {
            Color.clear
        } else {
            // A dog-ear's corner is folded back: the navy shows where it was.
            let earShown = scene == .plan && progress == 0 && FoldLeaf.sides(pair.settings.mode).contains(side)
            Paper(facet: PaperFacet(side))
                .clipShape(CornerCut(side: side, size: earShown ? earSize(side) : 0))
        }
    }

    /// A dog-ear's size: larger under the pointer when it can fold.
    private func earSize(_ side: FolderSide) -> CGFloat {
        hoveredEar == side && canFold ? 26 : 16
    }

    private func header(_ progress: Double) -> some View {
        // The mode is said above the paper, not printed on its fold.
        HStack(spacing: 0) {
            FoldFolderLabel(viewModel: viewModel, folder: pair.settings.left, side: .a)
                .opacity(pageInk(.a, progress))
                .environment(\.colorScheme, ground(.a))
            Color.clear.frame(width: FoldMetrics.creaseWidth)
            FoldFolderLabel(viewModel: viewModel, folder: pair.settings.right, side: .b)
                .opacity(pageInk(.b, progress))
                .environment(\.colorScheme, ground(.b))
        }
        .frame(height: Self.headerHeight)
    }

    /// What `side`'s label is printed on: its paper, or the navy where a
    /// missing folder's half would be, which needs the dark appearance.
    private func ground(_ side: FolderSide) -> ColorScheme {
        sync.missingSide == side ? .dark : colorScheme
    }

    /// The page's body. The line list keeps one identity while a plan is
    /// shown, re-checked after a choice, or pressed, so its scroll
    /// position stays put.
    @ViewBuilder
    private func content(scene: FoldScene, entries: [FoldEntry], progress: Double) -> some View {
        Group {
            if scene == .imprint {
                // Stamped on the page the leaf pressed, mid-page or above
                // the failures it lists, once the leaf is open: the card is
                // on the paper, and a lifted half has none.
                VStack(spacing: 0) {
                    if let imprint = sync.imprint, progress == 0 {
                        onPage(FoldLeaf.landing(leafSide)) {
                            VStack(spacing: 8) {
                                FoldImprintView(imprint: imprint)
                                if let note = sync.archiveNote {
                                    Text(note)
                                        .font(.caption)
                                        .foregroundStyle(FoldPalette.umber)
                                        .multilineTextAlignment(.center)
                                        .textSelection(.enabled)
                                }
                            }
                            .padding(.top, 18)
                            .padding(.bottom, 10)
                            .padding(.horizontal, 12)
                        }
                        .transition(.asymmetric(insertion: .opacity.combined(with: .scale(scale: 1.1)),
                                                removal: .opacity.combined(with: .offset(y: -10))))
                        .id(imprint.title)
                        // Its notes wrap on a narrow page; failures listed
                        // under it take the room that is left.
                        .layoutPriority(1)
                    }
                    if !entries.isEmpty { lineList(entries, scene: scene, progress: progress) }
                }
                .frame(maxHeight: .infinity, alignment: entries.isEmpty ? .center : .top)
                // A stamp lands once and stays: no bounce.
                .animation(reduceMotion ? nil : .spring(duration: 0.22 * timeScale, bounce: 0), value: progress == 0)
            } else if scene == .lifting {
                onPage(messagePage) {
                    VStack(spacing: 10) {
                        ProgressView().controlSize(.small)
                        Text(sync.statusText)
                            .font(.callout)
                            .foregroundStyle(FoldPalette.umber)
                            .multilineTextAlignment(.center)
                    }
                    .padding(24)
                    .frame(maxHeight: .infinity)
                }
            } else if !entries.isEmpty {
                lineList(entries, scene: scene, progress: progress)
                    .opacity(scene == .comparing ? 0.42 : 1)
                    .overlay {
                        if scene == .comparing { ComparingSweep(paused: reduceMotion, timeScale: timeScale) }
                    }
            } else if scene == .comparing {
                onPage(messagePage) {
                    VStack(spacing: 10) {
                        Text(sync.statusText)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(FoldPalette.umber)
                            .multilineTextAlignment(.center)
                            .accessibilityAddTraits(.updatesFrequently)
                        if let fraction = sync.progressFraction {
                            ProgressView(value: fraction)
                                .tint(FoldPalette.umber)
                                .frame(maxWidth: 180)
                                .accessibilityLabel("Progress")
                        }
                    }
                    .padding(24)
                    .frame(maxHeight: .infinity)
                }
                .frame(maxHeight: .infinity)
                // Light may cross the gutter; print may not.
                .overlay { ComparingSweep(paused: reduceMotion, timeScale: timeScale) }
            } else if scene != .pressing {
                // A run attached to mid-press has no lines here; the leaf
                // covers one half and the navy shows through the other.
                placeholder
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(reduceMotion ? nil : .spring(duration: 0.45 * timeScale, bounce: 0.15), value: scene)
    }

    /// A message on one page: the page that is there when a folder is
    /// missing, else the page the pair writes to.
    @ViewBuilder
    private var placeholder: some View {
        if sync.showsSearch, rows.isEmpty, sync.placeholder == nil {
            onPage(messagePage) {
                ContentUnavailableView.search(text: sync.searchText)
            }
        } else if let placeholder = sync.placeholder {
            onPage(messagePage) {
                placeholderMessage(placeholder)
            }
        }
    }

    private func placeholderMessage(_ placeholder: PairPlaceholder) -> some View {
        VStack(spacing: 10) {
            Image(systemName: placeholder.symbol)
                .font(.system(size: 26, weight: .light))
                .foregroundStyle(FoldPalette.umber)
            Text(placeholder.title)
                .font(.system(size: 17, weight: .semibold))
                .multilineTextAlignment(.center)
            Text(placeholder.message)
                .font(.callout)
                .foregroundStyle(FoldPalette.umber)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 380)
                .textSelection(.enabled)
            if placeholder.action == .preview {
                Button("Preview Changes") { Task { await sync.preview() } }
                    .disabled(!sync.canStart)
                    .padding(.top, 4)
            }
        }
        .padding(24)
        .frame(maxHeight: .infinity)
    }

    // MARK: Lines

    private func lineList(_ entries: [FoldEntry], scene: FoldScene, progress: Double) -> some View {
        let lookup = scene.allowsInspection ? rowsByID : [:]
        let selectedID = scene.allowsInspection ? viewModel.selectedChangeID : nil
        return ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(Array(entries.enumerated()), id: \.element.id) { index, entry in
                    entryRow(entry, row: entry.lineID.flatMap { lookup[$0] }, scene: scene,
                             selected: entry.lineID != nil && entry.lineID == selectedID, progress: progress)
                        .modifier(InkIn(delay: Double(index) * Self.printStagger * timeScale,
                                        active: printing != nil && index < Self.printedLines))
                        .id(entry.id)
                }
            }
            .padding(.vertical, Self.listInset)
        }
        // A new plan re-inks the lines that changed in place.
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.25 * timeScale), value: entries)
        // Held still while the leaf is up, so the lines it carries stay
        // where they were.
        .scrollDisabled(progress > 0)
        // No scroller floating over the navy where a lifted page was.
        .scrollIndicators(progress > 0 ? .hidden : .automatic)
        .scrollContentBackground(.hidden)
        .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.y } action: { _, offset in
            scrollMark.offset = max(0, offset)
        }
    }

    @ViewBuilder
    private func entryRow(_ entry: FoldEntry, row: ChangeRow?, scene: FoldScene, selected: Bool,
                          progress: Double) -> some View {
        switch entry {
        case .folder(let path, let count):
            HStack(spacing: 0) {
                FoldHeadingHalf(path: path, count: count, side: .a)
                    .opacity(pageInk(.a, progress))
                Color.clear.frame(width: FoldMetrics.creaseWidth)
                FoldHeadingHalf(path: path, count: count, side: .b)
                    .opacity(pageInk(.b, progress))
            }
            .frame(height: FoldMetrics.rowHeight)
            // Both pages print the path; VoiceOver reads it once.
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(path)
            .accessibilityValue(FoldHeadingHalf.countText(count))
            .accessibilityAddTraits(.isHeader)
        case .line(let line):
            lineRow(line, row: row, scene: scene, selected: selected, progress: progress)
        }
    }

    private func lineRow(_ line: FoldLine, row: ChangeRow?, scene: FoldScene, selected: Bool,
                         progress: Double) -> some View {
        let stagedFlow = staged?.lineID == line.id ? staged?.flow : nil
        // Impressions on the page the leaf comes down on take its ink as
        // it approaches: a peek at the outcome before letting go.
        let peek = scene == .plan ? FoldMotion.peek(progress) : 0
        let turn = turnControl(line, scene: scene)
        return HStack(spacing: 0) {
            linePage(line, side: .a, staged: stagedFlow, turn: turn, selected: selected,
                     peek: peek, progress: progress)
            Color.clear.frame(width: FoldMetrics.creaseWidth)
            linePage(line, side: .b, staged: stagedFlow, turn: turn, selected: selected,
                     peek: peek, progress: progress)
        }
        .frame(height: FoldMetrics.rowHeight)
        // The gutter belongs to the line too: a click there selects it.
        .contentShape(Rectangle())
        .help(line.path)
        .onTapGesture { if scene.allowsInspection { select(line) } }
        .simultaneousGesture(TapGesture(count: 2).onEnded { if scene.allowsInspection { quickLook(row) } })
        .contextMenu {
            if let row { ChangeRowActions(viewModel: viewModel, sync: sync, row: row) }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.path)
        .accessibilityValue(Self.spoken(line))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("fold-line")
        .accessibilityAction { if scene.allowsInspection { select(line) } }
        .accessibilityActions {
            if let row {
                ChangeRowActions(viewModel: viewModel, sync: sync, row: row)
            }
        }
    }

    /// One page of a line, its selection drawn on that page alone so the
    /// gutter stays clear and a lifted page takes its own along.
    private func linePage(_ line: FoldLine, side: FolderSide, staged stagedFlow: FoldFlow?,
                          turn: FoldTurnControl?, selected: Bool, peek: Double, progress: Double) -> some View {
        FoldHalf(line: line, side: side, peek: leafSide == side ? 0 : peek, staged: stagedFlow,
                 shakes: shakes[line.id] ?? 0, turn: turn)
            .background {
                if selected { SelectionTint(focused: focus.wrappedValue == .changes) }
            }
            .opacity(pageInk(side, progress))
    }

    /// A click on a line's mark stages the next way it can turn, as ←
    /// and → do; nothing changes until Return or the desk's button.
    private func turnControl(_ line: FoldLine, scene: FoldScene) -> FoldTurnControl? {
        guard scene == .plan, !line.turns.isEmpty else { return nil }
        return FoldTurnControl(enabled: sync.canStart,
                               help: line.turns.values.sorted().joined(separator: " · "),
                               cycle: { cycleStage(line) })
    }

    /// What VoiceOver says after the path: the change, its size, and the
    /// absence an archive carries over.
    private static func spoken(_ line: FoldLine) -> String {
        var parts = [line.actionText]
        if !line.sizeText.isEmpty { parts.append(line.sizeText) }
        if line.a == .blank, line.b == .lift { parts.append(String(localized: "Folder A has no copy")) }
        if line.b == .blank, line.a == .lift { parts.append(String(localized: "Folder B has no copy")) }
        if let from = line.movedFrom { parts.append(String(localized: "renamed from \(from), no data copied")) }
        return parts.joined(separator: ", ")
    }

    // MARK: Keys

    private func select(_ line: FoldLine) {
        viewModel.selectedChangeID = line.id
        focus.wrappedValue = .changes
    }

    private var selectedLine: FoldLine? {
        guard let id = viewModel.selectedChangeID else { return nil }
        return rows.first { $0.id == id }.map(FoldLine.init)
    }

    /// ↑ and ↓ walk the lines in the order the sheet shows them: grouped
    /// by folder, not in raw path order.
    private func move(_ press: KeyPress, in entries: [FoldEntry], proxy: ScrollViewProxy) -> KeyPress.Result {
        guard plain(press), scene.allowsInspection, drag == nil else { return .ignored }
        let shown = entries.compactMap(\.line)
        guard !shown.isEmpty else { return .ignored }
        let step = press.key == .upArrow ? -1 : 1
        let current = shown.firstIndex { $0.id == viewModel.selectedChangeID }
        let next = current.map { min(max($0 + step, 0), shown.count - 1) } ?? (step > 0 ? 0 : shown.count - 1)
        viewModel.selectedChangeID = shown[next].id
        proxy.scrollTo(FoldEntry.line(shown[next]).id)
        return .handled
    }

    /// ← and → stage the turn that carries the line that way, or shake
    /// the pip when the line already flows so or cannot turn.
    private func stage(_ press: KeyPress) -> KeyPress.Result {
        guard plain(press), scene == .plan, let line = selectedLine else { return .ignored }
        let flow: FoldFlow = press.key == .leftArrow ? .toA : .toB
        guard line.flow != flow, let title = line.turns[flow], sync.canStart else {
            shake(line)
            return .handled
        }
        staged = StagedTurn(lineID: line.id, name: line.name, flow: flow, title: title)
        viewModel.play(.tick)
        return .handled
    }

    /// The pip's click: the first way the line can turn, then the next,
    /// then back to as it was.
    private func cycleStage(_ line: FoldLine) {
        select(line)
        let flows = line.turnFlows
        let next: FoldFlow?
        if let current = staged?.flow, staged?.lineID == line.id,
           let index = flows.firstIndex(of: current) {
            next = index + 1 < flows.count ? flows[index + 1] : nil
        } else {
            next = flows.first
        }
        guard let next, let title = line.turns[next] else {
            staged = nil
            return
        }
        staged = StagedTurn(lineID: line.id, name: line.name, flow: next, title: title)
        viewModel.play(.tick)
    }

    /// Run the staged turn, if the line still turns that way and the
    /// pair can take it.
    private func runStaged() -> KeyPress.Result {
        guard scene == .plan, sync.canStart, let turn = staged else { return .ignored }
        // A line that no longer turns that way lets go of the turn.
        guard let row = rowsByID[turn.lineID], FoldLine(row: row).turns[turn.flow] != nil else {
            staged = nil
            return .handled
        }
        staged = nil
        Task { await sync.turn(row, to: turn.flow) }
        return .handled
    }

    private func cancelStagedOrDrag() -> KeyPress.Result {
        if staged != nil {
            staged = nil
            return .handled
        }
        guard var current = drag, !current.cancelled, scene == .plan else { return .ignored }
        current.cancelled = true
        drag = current
        open()
        return .handled
    }

    private func shake(_ line: FoldLine) {
        // Without motion, the Mac's alert sound refuses instead.
        guard !reduceMotion else {
            NSSound.beep()
            return
        }
        withAnimation(.linear(duration: 0.28)) { shakes[line.id, default: 0] += 1 }
    }

    @discardableResult
    private func quickLook(_ row: ChangeRow?) -> KeyPress.Result {
        guard let url = row?.look else { return .ignored }
        viewModel.quickLookURL = url
        return .handled
    }

    private func skipSelected() -> KeyPress.Result {
        guard scene == .plan, let row = viewModel.selectedChange, row.item != nil, sync.canStart else {
            return .ignored
        }
        Task { await sync.skipOnce(row) }
        return .handled
    }

    // MARK: The leaf

    /// The halves a person may fold, each with its dog-ear.
    private func dogEars(scene: FoldScene, progress: Double) -> some View {
        let sides = FoldLeaf.sides(pair.settings.mode)
        return HStack(spacing: 0) {
            if sides.contains(.a) { ear(.a) }
            Spacer(minLength: 0)
            if sides.contains(.b) { ear(.b) }
        }
        .opacity(scene == .plan && progress == 0 ? 1 : 0)
        // Out of reach outside a plan. A lifting leaf only hides them: the
        // drag that lifts it is still under way on one.
        .allowsHitTesting(scene == .plan)
        .accessibilityHidden(scene != .plan)
    }

    private func ear(_ side: FolderSide) -> some View {
        let size = earSize(side)
        return ZStack(alignment: side == .a ? .topLeading : .topTrailing) {
            Color.clear.frame(width: 46, height: 46)
            // The flap shows the paper's cerulean back; the paper under
            // the corner it came from is cut away.
            DogEar(side: side)
                .fill(FoldPalette.kamiLit)
                .shadow(color: FoldPalette.foldShadow.opacity(0.3), radius: 1.5, x: side == .a ? 1 : -1, y: 1)
                .frame(width: size, height: size)
        }
        .contentShape(Rectangle())
        .opacity(canFold ? 1 : 0.4)
        .onHover { inside in hoveredEar = inside ? side : nil }
        .pointerStyle(!canFold ? nil : drag == nil ? PointerStyle.grabIdle : PointerStyle.grabActive)
        .animation(reduceMotion ? nil : Self.earSpring, value: hoveredEar)
        .gesture(foldDrag(side))
        .help(foldHint)
        .accessibilityElement()
        .accessibilityLabel(side == .a ? "Fold Folder A’s Half to Sync" : "Fold Folder B’s Half to Sync")
        .accessibilityHint(foldHint)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { foldAndSync(side, voice: .audible) }
    }

    private func foldDrag(_ side: FolderSide) -> some Gesture {
        DragGesture(minimumDistance: 0)
            .updating($pointerDown) { _, down, _ in down = true }
            .onChanged { value in
                if drag == nil {
                    // A drag that cannot lift the leaf now, or catches it
                    // still swinging open, stays dead until let go rather
                    // than jump to the pointer.
                    guard scene == .plan, !leafDown, approval == nil, !opening else {
                        drag = LeafDrag(side: side, token: sync.foldPlan?.token, cancelled: true)
                        return
                    }
                    drag = LeafDrag(side: side, token: sync.foldPlan?.token)
                    leafSide = side
                    focus.wrappedValue = .changes
                }
                guard var current = drag, !current.cancelled else { return }
                // The plan changed under the pointer: let go of it.
                if current.token != sync.foldPlan?.token {
                    current.cancelled = true
                    drag = current
                    open()
                    return
                }
                let travel = side == .a ? value.translation.width : -value.translation.width
                var next = FoldMotion.swing(travel: travel, width: halfWidth)
                if !canFold { next = min(next, FoldMotion.resistance) }
                if swing == 0, next > 0, canFold { viewModel.play(.swish) }
                if (next >= FoldMotion.tippingPoint) != (swing >= FoldMotion.tippingPoint) { tips += 1 }
                swing = next
            }
            .onEnded { _ in
                guard let current = drag else { return }
                drag = nil
                // Bound to the plan it began on, checked again at release.
                guard !current.cancelled, current.token == sync.foldPlan?.token else {
                    // Only a leaf this drag held opens; one falling for a
                    // run or held for approval stays.
                    if scene == .plan, !leafDown, approval == nil { open() }
                    return
                }
                if canFold, FoldMotion.commits(swing: swing) {
                    foldAndSync(current.side, voice: .audible)
                } else {
                    open()
                }
            }
    }

    /// Fold `side`'s half and sync the plan. A plan that removes enough
    /// to need approval holds the leaf upright and asks first.
    private func foldAndSync(_ side: FolderSide?, voice: FoldVoice) {
        guard canFold else {
            open()
            return
        }
        // A leaf still swinging open folds again from where it is; which
        // half folds is only how the sync looks.
        if !opening { leafSide = side ?? FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? []) }
        self.voice = voice
        if let request = sync.foldApproval {
            approval = request
            withAnimation(reduceMotion ? nil : .spring(duration: 0.35 * timeScale, bounce: 0.2)) {
                swing = FoldMotion.heldUpright
            }
            return
        }
        fall()
        Task {
            await viewModel.syncInWindow(pair.id)
            settle()
        }
    }

    /// The approval answered yes: fold the plan it was asked about, if
    /// that is still the plan on the sheet.
    private func approve(_ request: ApprovalRequest) {
        guard scene == .plan, sync.foldApproval?.reviewToken == request.reviewToken else {
            cancelApproval()
            return
        }
        approval = nil
        fall()
        Task {
            await viewModel.approveInWindow(request)
            settle()
        }
    }

    private func cancelApproval() {
        guard approval != nil else { return }
        approval = nil
        open()
    }

    /// Let the leaf fall onto the other half and press.
    private func fall() {
        motion += 1
        let generation = motion
        opening = false
        leafDown = true
        landed = false
        let animation: Animation = reduceMotion
            ? .easeInOut(duration: 0.2 * timeScale)
            : drop(over: 1 - swing, base: Self.fallTime)
        withAnimation(animation) {
            swing = 1
        } completion: {
            guard motion == generation, leafDown else { return }
            press()
        }
    }

    /// The landing: a thump through the trackpad, the sheet giving a
    /// little under the press.
    private func press() {
        landed = true
        // A fold no one made here lands without a thump or a sound.
        if voice == .audible {
            presses += 1
            viewModel.play(.press)
        }
        guard !reduceMotion else { return }
        let generation = motion
        withAnimation(.easeOut(duration: 0.07 * timeScale)) {
            bump = true
        } completion: {
            guard motion == generation else { return }
            withAnimation(.spring(duration: 0.28 * timeScale, bounce: 0)) { bump = false }
        }
    }

    /// The leaf falling under its own weight: it gathers speed and is
    /// still moving when it meets the page, so contact and thump land
    /// together. A longer drop takes longer, as a falling sheet does.
    private func drop(over travel: Double, base: Double) -> Animation {
        let duration = max(Self.shortestDrop, base * (travel / FoldMotion.tippingPoint).squareRoot())
        return .timingCurve(0.6, 0, 0.85, 0.75, duration: duration * timeScale)
    }

    /// Back open: dropped back short of a fold, or peeled off the
    /// pressed half after a run, rising to upright and dropping open.
    private func open(_ way: Opening = .fallBack) {
        motion += 1
        let generation = motion
        opening = opening || swing > 0
        leafDown = false
        landed = false
        bump = false
        voice = .silent
        if reduceMotion {
            withAnimation(.easeInOut(duration: 0.2 * timeScale)) { swing = 0 } completion: {
                if motion == generation { opening = false }
            }
            return
        }
        guard way == .peel, swing > FoldMotion.tippingPoint else {
            withAnimation(drop(over: swing, base: Self.fallBackTime)) { swing = 0 } completion: {
                if motion == generation { opening = false }
            }
            return
        }
        withAnimation(.timingCurve(0.4, 0, 0.6, 1, duration: Self.peelLift * timeScale)) {
            swing = FoldMotion.tippingPoint
        } completion: {
            guard motion == generation else { return }
            withAnimation(drop(over: FoldMotion.tippingPoint, base: Self.peelDrop)) { swing = 0 } completion: {
                if motion == generation { opening = false }
            }
        }
    }

    /// The run ended, or never started: open, unless a run still presses
    /// or a person is still holding the leaf or answering for it.
    private func settle() {
        guard scene != .pressing, approval == nil, drag == nil || drag?.cancelled == true,
              swing > 0 else { return }
        if voice == .audible { viewModel.play(.peel) }
        open(.peel)
    }

    private func sceneChanged(from old: FoldScene, to new: FoldScene) {
        let interacting = drag != nil || approval != nil
        if new != .plan {
            staged = nil
            approval = nil
            if var current = drag {
                current.cancelled = true
                drag = current
            }
        }
        if new == .pressing {
            if !leafDown {
                // A leaf already up keeps its side rather than jump the crease.
                if swing == 0, !opening {
                    leafSide = FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? [])
                }
                voice = .silent
                fall()
            }
        } else if old == .pressing || (new != .plan && interacting) {
            settle()
        }
        // The pressed plan is spent, and one that went out of date or was
        // undone is gone; a later comparison must not show it.
        if (old == .pressing && new != .pressing) || new == .message || new == .lifting {
            plan = sync.foldPlan
            shown = []
        }
        if new == .plan { shown = rows.map(FoldLine.init) }
        announce(new)
    }

    /// Print `token`'s plan: its first lines spread in, then the sheet
    /// stops animating rows that scroll into view.
    private func printPlan(_ token: String) {
        printing = token
        let lasting = (Double(Self.printedLines) * Self.printStagger + 0.35) * timeScale
        Task {
            try? await Task.sleep(for: .seconds(lasting))
            if printing == token { printing = nil }
        }
    }

    private func announce(_ scene: FoldScene) {
        let text: String? = switch scene {
        case .plan: sync.shortStatus
        case .pressing: String(localized: "Syncing")
        case .imprint: sync.imprint.map { imprint in
            ([imprint.title] + imprint.figures.map { "\($0.value) \($0.label)" } + imprint.notes)
                .joined(separator: ", ")
        }
        case .lifting: String(localized: "Undoing the last sync")
        case .comparing, .message: nil
        }
        if let text { AccessibilityNotification.Announcement(text).post() }
    }

    /// What the folded leaf's cerulean back says while the run presses,
    /// in white: the run's own progress, never a claim about any one line.
    private var pressReadout: some View {
        VStack(spacing: 10) {
            Text(pair.settings.mode.title)
                .font(FoldType.backEyebrow)
                .textCase(.uppercase)
                .tracking(FoldType.eyebrowTracking * 1.5)
                .foregroundStyle(.white)
            Text(sync.progressFraction.map { $0.formatted(.percent.precision(.fractionLength(0))) }
                 ?? String(localized: "Pressing"))
                .font(FoldType.figure)
                .foregroundStyle(.white)
                .contentTransition(.numericText())
            Group {
                if let fraction = sync.progressFraction {
                    ProgressView(value: fraction)
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
            }
            .tint(.white)
            .frame(maxWidth: 220)
            .accessibilityLabel("Progress")
            Text(sync.statusText)
                .font(.callout)
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
                .lineLimit(3)
                .accessibilityAddTraits(.updatesFrequently)
            Button("Stop") { Task { await sync.cancel() } }
                .disabled(!sync.canCancel)
        }
        // Controls drawn for a dark ground, legible on the cerulean.
        .environment(\.colorScheme, .dark)
        .padding(16)
        .frame(width: halfWidth)
        .frame(maxHeight: .infinity)
        // Printed on a flat plate over the folded back: no facet ridge
        // runs through the words.
        .background { Kami(finish: .flat) }
        .offset(x: leafSide == .a ? halfWidth : 0)
    }

    // MARK: Desk

    /// The strip under the sheet: what the sheet says in words, and what
    /// can follow it. It lies on the navy, so it is drawn for a dark
    /// ground in either appearance.
    private func desk(scene: FoldScene) -> some View {
        // Buttons keep their titles. The words sit beside them while they
        // have room to say something, and above them when they do not.
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: 12) {
                deskText(scene: scene)
                    .frame(minWidth: Self.deskTextRoom, idealWidth: Self.deskTextRoom, maxWidth: .infinity,
                           alignment: .leading)
                deskButtons(scene: scene)
                    .fixedSize()
            }
            VStack(alignment: .trailing, spacing: 6) {
                deskText(scene: scene)
                    .frame(maxWidth: .infinity, alignment: .leading)
                deskButtons(scene: scene)
                    .fixedSize()
            }
        }
        .frame(minHeight: 28)
        .environment(\.colorScheme, .dark)
    }

    @ViewBuilder
    private func deskText(scene: FoldScene) -> some View {
        if let turn = staged {
            Label {
                // The turn's name stays whole; only the file's name shortens.
                HStack(spacing: 4) {
                    Text("\(turn.title):")
                        .fixedSize()
                    Text(turn.name)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            } icon: {
                Image(systemName: turn.flow == .toA ? "arrow.left" : "arrow.right")
            }
            .font(.callout.weight(.medium))
            .foregroundStyle(FoldPalette.ink(turn.flow == .toA ? .b : .a))
        } else if let error = sync.requestError {
            Label(error, systemImage: "exclamationmark.octagon")
                .font(.callout)
                .foregroundStyle(.red)
                .lineLimit(2)
                .textSelection(.enabled)
        } else if scene == .plan || scene == .imprint {
            Text(sync.statusText)
                .font(.callout)
                .foregroundStyle(FoldPalette.deskInk)
                .lineLimit(2)
                .help(sync.statusText)
                .textSelection(.enabled)
        }
    }

    @ViewBuilder
    private func deskButtons(scene: FoldScene) -> some View {
        HStack(spacing: 8) {
            if sync.isWorking, scene != .pressing {
                // The leaf's back carries Stop while a run presses.
                Button("Stop") { Task { await sync.cancel() } }
                    .disabled(!sync.canCancel)
            } else if let turn = staged {
                Button("Cancel") { staged = nil }
                Button(turn.title) { _ = runStaged() }
                    .buttonStyle(.borderedProminent)
                    .disabled(scene != .plan || !sync.canStart)
                    .help("Return")
            } else if scene == .plan, canFold {
                Button {
                    foldAndSync(nil, voice: .audible)
                } label: {
                    Label("Sync", systemImage: "arrow.triangle.2.circlepath")
                }
                .help("Fold the sheet and sync (⌘S)")
            } else if scene == .imprint {
                if let card = sync.completionCard, card.conflicts > 0 {
                    Button(card.conflicts == 1 ? "Review 1 Conflict" : "Review \(card.conflicts) Conflicts") {
                        Task { await sync.reviewConflicts() }
                    }
                    .disabled(!sync.canStart)
                }
                if sync.canUndo {
                    Button("Undo Sync") { Task { await viewModel.undoSync(pairID: pair.id) } }
                        .help("Lift this sync: put back what it replaced or archived, and archive what it created.")
                }
                Button("Preview Again") { Task { await sync.preview() } }
                    .disabled(!sync.canStart)
            }
        }
    }
}

/// Whether the fold follows Reduce Motion.
enum FoldMotionPolicy {
    case system
    /// Swing regardless: a Debug tour filming the leaf.
    case full
    /// Exercise Reduce Motion regardless of the test runner's preference.
    case reduced
}

extension EnvironmentValues {
    /// How much slower the fold moves than it should: 1, except in a
    /// Debug tour that keeps frames of it in motion.
    @Entry var foldTimeScale: Double = 1
    @Entry var foldMotionPolicy: FoldMotionPolicy = .system
}

private extension FoldEntry {
    /// The line an entry is, if it is one.
    var line: FoldLine? {
        if case .line(let line) = self { return line }
        return nil
    }

    /// The change row a line entry stands for.
    var lineID: ChangeRow.ID? { line?.id }
}
