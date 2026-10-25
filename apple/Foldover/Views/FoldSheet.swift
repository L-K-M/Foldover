// The selected pair's sheet (PLAN.md §3.8): its plan printed across a
// creased page, folded to sync.
//
//   ┌──────────────── paper ─────────────────┐
//   │◤ Folder A         ⇄          Folder B ◥│  dog-ears: drag to fold
//   │   2026/Trip/day2  │  3 changes          │
//   │    IMG_2041.RAW   →   IMG_2041.RAW  copy│  ink │ impression
//   │      header.psd   ●   header.psd   both │  a pin holds conflicts
//   └─────────────────────────────────────────┘
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

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.foldTimeScale) private var timeScale

    /// How far the leaf has swung: 0 open … 1 folded flat.
    @State private var swing: Double = 0
    @State private var leafSide: FolderSide = .a
    /// The leaf is down, or on its way, for a run; it opens when the run ends.
    @State private var leafDown = false
    @State private var voice = FoldVoice.silent
    @State private var drag: LeafDrag?
    /// The plan the sheet last showed, for the leaf to carry and for the
    /// re-preview after a choice to keep on screen.
    @State private var plan: FoldPlan?
    @State private var paperSize: CGSize = .zero
    @State private var scrollOffset: CGFloat = 0
    @State private var staged: StagedTurn?
    @State private var approval: ApprovalRequest?
    @State private var hoveredEar: FolderSide?
    /// Haptic and motion triggers.
    @State private var tips = 0
    @State private var presses = 0
    @State private var bump = false
    @State private var nudge = Nudge()

    private static let space = "fold-sheet"
    private static let headerHeight: CGFloat = 50
    private static let listInset: CGFloat = 6
    /// Which way a positive angle turns: the leaf's free edge rises
    /// toward the viewer.
    private static let turnSign: Double = -1

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

    /// A shake for a line that cannot turn the way asked.
    private struct Nudge: Equatable {
        var lineID: ChangeRow.ID?
        var count = 0
    }

    var body: some View {
        let scene = self.scene
        let lines = lines(for: scene)
        VStack(spacing: 10) {
            paper(scene: scene, lines: lines)
                .scaleEffect(bump ? 0.985 : 1)
            desk(scene: scene)
        }
        .padding(.horizontal, 14)
        .padding(.bottom, 12)
        .onChange(of: sync.foldPlan, initial: true) { _, new in
            if let new { plan = new }
        }
        .onChange(of: plan?.token) { staged = nil }
        .onChange(of: viewModel.selectedChangeID) { staged = nil }
        .onChange(of: scene) { old, new in sceneChanged(from: old, to: new) }
        // A run already pressing when the pair is shown: the leaf is down.
        .onAppear {
            guard scene == .pressing else { return }
            leafSide = FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? [])
            leafDown = true
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
        case .comparing, .pressing: plan?.lines ?? []
        case .lifting, .message: []
        }
    }

    private var rowsByID: [ChangeRow.ID: ChangeRow] {
        Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    private var halfWidth: CGFloat { paperSize.width / 2 }
    private var compact: Bool { halfWidth < FoldMetrics.compactHalf }

    /// A person may fold a plan with lines while the pair can sync and
    /// no turn waits for Return.
    private var canFold: Bool {
        scene == .plan && sync.canSync && !(plan?.lines.isEmpty ?? true) && staged == nil
    }

    private var foldHint: String {
        if staged != nil { return String(localized: "Confirm or cancel the change you turned first.") }
        if plan?.lines.isEmpty ?? true { return String(localized: "Nothing to sync. Preview to check again.") }
        if !sync.canSync { return String(localized: "Preview first, or wait until the pair is idle.") }
        return String(localized: "Drag toward the middle to fold the sheet and sync, or press ⌘S.")
    }

    /// The half lifted off the sheet as the leaf.
    private func lifted(_ side: FolderSide) -> Bool { swing > 0 && leafSide == side }

    // MARK: Paper

    private func paper(scene: FoldScene, lines: [FoldLine]) -> some View {
        VStack(spacing: 0) {
            header
            content(scene: scene, lines: lines)
        }
        // The page's shadow falls on the desk from the paper alone, not
        // from every line of text on it.
        .background { halves.compositingGroup().shadow(color: .black.opacity(0.16), radius: 5, x: 0, y: 2) }
        .overlay { Crease(depth: swing) }
        .overlay(alignment: .topLeading) { leafLayer(lines: lines) }
        .overlay(alignment: .topLeading) {
            if scene == .pressing, swing >= 0.98 { pressReadout }
        }
        .overlay(alignment: .top) { dogEars(scene: scene) }
        .onGeometryChange(for: CGSize.self) { $0.size } action: { paperSize = $0 }
        .coordinateSpace(.named(Self.space))
    }

    /// The two halves of the page: paper, the desk where the leaf lifted
    /// off, or a dashed outline where a folder is not connected.
    private var halves: some View {
        HStack(spacing: 0) {
            half(.a)
            half(.b)
        }
    }

    @ViewBuilder
    private func half(_ side: FolderSide) -> some View {
        if sync.missingSide == side {
            RoundedRectangle(cornerRadius: 4)
                .strokeBorder(FoldPalette.pencil.opacity(0.55), style: StrokeStyle(lineWidth: 1, dash: [6, 4]))
                .padding(3)
        } else if lifted(side) {
            Color.clear
        } else {
            Paper()
        }
    }

    private var header: some View {
        HStack(spacing: 0) {
            FoldFolderLabel(viewModel: viewModel, folder: pair.settings.left, side: .a)
                .opacity(lifted(.a) ? 0 : 1)
            Image(systemName: pair.settings.mode.symbol)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(FoldPalette.pencil)
                .frame(width: FoldMetrics.creaseWidth)
                .help(pair.settings.mode.title)
                .accessibilityLabel(pair.settings.mode.title)
            FoldFolderLabel(viewModel: viewModel, folder: pair.settings.right, side: .b)
                .opacity(lifted(.b) ? 0 : 1)
        }
        .frame(height: Self.headerHeight)
    }

    /// The page's body. The line list keeps one identity while a plan is
    /// shown, re-checked after a choice, or pressed, so its scroll
    /// position and the keys stay put.
    @ViewBuilder
    private func content(scene: FoldScene, lines: [FoldLine]) -> some View {
        Group {
            if scene == .imprint {
                VStack(spacing: 0) {
                    if let imprint = sync.imprint {
                        FoldImprintView(imprint: imprint, ink: imprintInk)
                            .padding(.top, 18)
                            .padding(.bottom, 10)
                            .transition(.asymmetric(insertion: .opacity.combined(with: .scale(scale: 1.04)),
                                                    removal: .opacity.combined(with: .offset(y: -10))))
                            .id(imprint.title)
                    }
                    if !lines.isEmpty { lineList(lines, interactive: false) }
                    Spacer(minLength: 0)
                }
            } else if scene == .lifting {
                VStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text(sync.statusText)
                        .font(.callout)
                        .foregroundStyle(FoldPalette.pencil)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if !lines.isEmpty {
                lineList(lines, interactive: scene == .plan)
                    .opacity(scene == .comparing ? 0.42 : 1)
                    .overlay {
                        if scene == .comparing { ComparingSweep(paused: reduceMotion) }
                    }
            } else if scene == .comparing {
                VStack(spacing: 10) {
                    Text(sync.statusText)
                        .font(.system(size: 13, design: .serif).italic())
                        .foregroundStyle(FoldPalette.pencil)
                        .multilineTextAlignment(.center)
                        .accessibilityAddTraits(.updatesFrequently)
                    if let fraction = sync.progressFraction {
                        ProgressView(value: fraction)
                            .tint(FoldPalette.pencil)
                            .frame(width: 180)
                            .accessibilityLabel("Progress")
                    }
                }
                .padding(24)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay { ComparingSweep(paused: reduceMotion) }
            } else {
                placeholder
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .animation(reduceMotion ? nil : .spring(duration: 0.45 * timeScale, bounce: 0.15), value: scene)
    }

    /// The imprint's ink: a one-way run prints in its source's ink, a
    /// two-way one in neutral pencil, since both inks crossed.
    private var imprintInk: Color {
        switch pair.settings.mode {
        case .sync: FoldPalette.pencil
        case .mirrorLeft, .backupLeft: FoldPalette.ink(.a)
        case .mirrorRight, .backupRight: FoldPalette.ink(.b)
        }
    }

    @ViewBuilder
    private var placeholder: some View {
        if sync.showsSearch, rows.isEmpty, sync.placeholder == nil {
            ContentUnavailableView.search(text: sync.searchText)
        } else if let placeholder = sync.placeholder {
            VStack(spacing: 10) {
                Image(systemName: placeholder.symbol)
                    .font(.system(size: 26, weight: .light))
                    .foregroundStyle(FoldPalette.pencil)
                Text(placeholder.title)
                    .font(.system(size: 17, design: .serif))
                Text(placeholder.message)
                    .font(.callout)
                    .foregroundStyle(FoldPalette.pencil)
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
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    // MARK: Lines

    private func lineList(_ lines: [FoldLine], interactive: Bool) -> some View {
        let entries = FoldEntry.entries(lines)
        let lookup = interactive ? rowsByID : [:]
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(entries) { entry in
                        entryRow(entry, row: entry.lineID.flatMap { lookup[$0] }, interactive: interactive)
                            .id(entry.id)
                    }
                }
                .padding(.vertical, Self.listInset)
            }
            // A new plan re-inks the lines that changed in place.
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.25 * timeScale), value: lines)
            .scrollDisabled(swing > 0)
            .scrollContentBackground(.hidden)
            .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.y } action: { _, offset in
                scrollOffset = max(0, offset)
            }
            // Focusable while re-checking too, so a turn keeps the keys.
            .focusable()
            .focused(focus, equals: .changes)
            .focusEffectDisabled()
            .onKeyPress(.upArrow) { move(-1, in: lines, proxy: proxy) }
            .onKeyPress(.downArrow) { move(1, in: lines, proxy: proxy) }
            .onKeyPress(.leftArrow) { stage(.toA, in: lines) }
            .onKeyPress(.rightArrow) { stage(.toB, in: lines) }
            .onKeyPress(.return) { runStaged() }
            .onKeyPress(.escape) { cancelStagedOrDrag() }
            .onKeyPress(.space) { quickLook(viewModel.selectedChange) }
            .onKeyPress(keys: [.delete, .deleteForward]) { _ in skipSelected() }
            // Edit > Copy copies the selected change's path.
            .onCopyCommand {
                guard let path = viewModel.selectedChange?.path else { return [] }
                return [NSItemProvider(object: path as NSString)]
            }
        }
    }

    @ViewBuilder
    private func entryRow(_ entry: FoldEntry, row: ChangeRow?, interactive: Bool) -> some View {
        switch entry {
        case .folder(let path, let count):
            HStack(spacing: 0) {
                FoldHeadingHalf(path: path, count: count, side: .a)
                    .opacity(lifted(.a) ? 0 : 1)
                Image(systemName: "folder")
                    .font(.system(size: 8))
                    .foregroundStyle(FoldPalette.pencil.opacity(0.7))
                    .frame(width: FoldMetrics.creaseWidth)
                    .padding(.top, 6)
                FoldHeadingHalf(path: path, count: count, side: .b)
                    .opacity(lifted(.b) ? 0 : 1)
            }
            .frame(height: FoldMetrics.rowHeight)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
        case .line(let line):
            lineRow(line, row: row, interactive: interactive)
        }
    }

    private func lineRow(_ line: FoldLine, row: ChangeRow?, interactive: Bool) -> some View {
        let selected = interactive && viewModel.selectedChangeID == line.id
        let stagedFlow = staged?.lineID == line.id ? staged?.flow : nil
        return HStack(spacing: 0) {
            FoldHalf(line: line, side: .a, compact: compact, peek: peek(.a))
                .opacity(lifted(.a) ? 0 : 1)
            pip(line, staged: stagedFlow, interactive: interactive)
                .frame(width: FoldMetrics.creaseWidth)
                .modifier(Shake(count: nudge.lineID == line.id ? nudge.count : 0))
            FoldHalf(line: line, side: .b, compact: compact, peek: peek(.b))
                .opacity(lifted(.b) ? 0 : 1)
        }
        .frame(height: FoldMetrics.rowHeight)
        .background {
            if selected { Highlighter(focused: focus.wrappedValue == .changes) }
        }
        .contentShape(Rectangle())
        .onTapGesture { if interactive { select(line) } }
        .simultaneousGesture(TapGesture(count: 2).onEnded { if interactive { quickLook(row) } })
        .contextMenu {
            if let row { ChangeRowActions(viewModel: viewModel, sync: sync, row: row) }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.path)
        .accessibilityValue(Self.spoken(line))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("fold-line")
        .accessibilityActions {
            if let row, interactive {
                ForEach(line.turnFlows, id: \.self) { flow in
                    Button(line.turns[flow] ?? "") { Task { await sync.turn(row, to: flow) } }
                }
                if row.keepsBoth {
                    Button("Keep Both Versions") { Task { await sync.keepBoth(for: row) } }
                }
                if row.look != nil {
                    Button("Quick Look") { quickLook(row) }
                }
                if row.item != nil {
                    Button("Skip Once") { Task { await sync.skipOnce(row) } }
                }
            }
        }
    }

    /// The pip. Clicking it stages the next way the line can turn, as ←
    /// and → do; nothing changes until Return or the desk's button.
    @ViewBuilder
    private func pip(_ line: FoldLine, staged stagedFlow: FoldFlow?, interactive: Bool) -> some View {
        if interactive, !line.turns.isEmpty {
            Button { cycleStage(line) } label: {
                FoldPip(flow: line.flow, staged: stagedFlow)
            }
            .buttonStyle(.plain)
            .disabled(!sync.canStart)
            .help(line.turns.values.sorted().joined(separator: " · "))
            .accessibilityHidden(true)
        } else {
            FoldPip(flow: line.flow, staged: stagedFlow)
        }
    }

    /// Impressions on the half the leaf comes down on take its ink as it
    /// approaches: a peek at the outcome before letting go.
    private func peek(_ side: FolderSide) -> Double {
        guard swing > 0, leafSide != side, scene == .plan else { return 0 }
        return min(1, swing * 1.6)
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

    private func move(_ step: Int, in lines: [FoldLine], proxy: ScrollViewProxy) -> KeyPress.Result {
        guard scene == .plan, !lines.isEmpty else { return .ignored }
        let current = lines.firstIndex { $0.id == viewModel.selectedChangeID }
        let next = current.map { min(max($0 + step, 0), lines.count - 1) } ?? (step > 0 ? 0 : lines.count - 1)
        viewModel.selectedChangeID = lines[next].id
        proxy.scrollTo(FoldEntry.line(lines[next]).id)
        return .handled
    }

    /// ← and → stage the turn that carries the line that way, or shake
    /// the pip when the line already flows so or cannot turn.
    private func stage(_ flow: FoldFlow, in lines: [FoldLine]) -> KeyPress.Result {
        guard scene == .plan, let line = selectedLine else { return .ignored }
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

    private func runStaged() -> KeyPress.Result {
        guard let turn = staged, let row = rowsByID[turn.lineID] else { return .ignored }
        staged = nil
        Task { await sync.turn(row, to: turn.flow) }
        return .handled
    }

    private func cancelStagedOrDrag() -> KeyPress.Result {
        if staged != nil {
            staged = nil
            return .handled
        }
        guard var current = drag else { return .ignored }
        current.cancelled = true
        drag = current
        open()
        return .handled
    }

    private func shake(_ line: FoldLine) {
        withAnimation(reduceMotion ? nil : .linear(duration: 0.28)) {
            nudge = Nudge(lineID: line.id, count: nudge.count + 1)
        }
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
    private func dogEars(scene: FoldScene) -> some View {
        let sides = FoldLeaf.sides(pair.settings.mode)
        return HStack(spacing: 0) {
            if sides.contains(.a) { ear(.a) }
            Spacer(minLength: 0)
            if sides.contains(.b) { ear(.b) }
        }
        .opacity(scene == .plan && swing == 0 ? 1 : 0)
    }

    private func ear(_ side: FolderSide) -> some View {
        let size: CGFloat = hoveredEar == side && canFold ? 26 : 16
        return ZStack(alignment: side == .a ? .topLeading : .topTrailing) {
            Color.clear.frame(width: 46, height: 46)
            ZStack {
                DogEar(side: side, part: .cut).fill(FoldPalette.desk)
                DogEar(side: side, part: .flap)
                    .fill(FoldPalette.paperBack)
                    .shadow(color: .black.opacity(0.28), radius: 1.5, x: side == .a ? 1 : -1, y: 1)
            }
            .frame(width: size, height: size)
        }
        .contentShape(Rectangle())
        .opacity(canFold ? 1 : 0.4)
        .onHover { inside in hoveredEar = inside ? side : nil }
        .animation(reduceMotion ? nil : .spring(duration: 0.22, bounce: 0.3), value: hoveredEar)
        .gesture(foldDrag(side))
        .help(foldHint)
        .accessibilityElement()
        .accessibilityLabel(side == .a ? "Fold Folder A’s Half to Sync" : "Fold Folder B’s Half to Sync")
        .accessibilityHint(foldHint)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { foldAndSync(side, voice: .silent) }
    }

    private func foldDrag(_ side: FolderSide) -> some Gesture {
        DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.space))
            .onChanged { value in
                if drag == nil {
                    guard scene == .plan, !leafDown else { return }
                    drag = LeafDrag(side: side, token: plan?.token)
                    leafSide = side
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
                guard !current.cancelled else { return }
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
        leafSide = side ?? FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? [])
        if let request = sync.foldApproval {
            approval = request
            withAnimation(reduceMotion ? nil : .spring(duration: 0.35 * timeScale, bounce: 0.2)) {
                swing = FoldMotion.heldUpright
            }
            return
        }
        self.voice = voice
        fall()
        Task {
            await viewModel.syncInWindow(pair.id)
            settle()
        }
    }

    private func approve(_ request: ApprovalRequest) {
        approval = nil
        voice = .audible
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
        leafDown = true
        guard !reduceMotion else {
            withAnimation(.easeInOut(duration: 0.2 * timeScale)) { swing = 1 }
            press()
            return
        }
        withAnimation(.spring(duration: 0.42 * timeScale, bounce: 0.05)) {
            swing = 1
        } completion: {
            press()
        }
    }

    /// The landing: a thump through the trackpad, the sheet giving a
    /// little under the press.
    private func press() {
        guard leafDown else { return }
        presses += 1
        if voice == .audible { viewModel.play(.press) }
        guard !reduceMotion else { return }
        withAnimation(.easeOut(duration: 0.07 * timeScale)) {
            bump = true
        } completion: {
            withAnimation(.spring(duration: 0.3 * timeScale, bounce: 0.45)) { bump = false }
        }
    }

    /// Back open, short of a fold or after a run.
    private func open() {
        leafDown = false
        withAnimation(reduceMotion ? .easeInOut(duration: 0.2 * timeScale)
                      : .spring(duration: 0.55 * timeScale, bounce: 0.18)) {
            swing = 0
        }
    }

    /// The window's sync returned: the run ended, or never started. Open
    /// unless a run is still pressing.
    private func settle() {
        guard scene != .pressing, drag == nil, swing > 0 else { return }
        if voice == .audible { viewModel.play(.peel) }
        voice = .silent
        open()
    }

    private func sceneChanged(from old: FoldScene, to new: FoldScene) {
        if new != .plan { staged = nil }
        if new == .pressing, !leafDown, drag == nil {
            leafSide = FoldLeaf.automatic(pair.settings.mode, lines: plan?.lines ?? [])
            approval = nil
            fall()
        }
        if old == .pressing, new != .pressing {
            settle()
            // The pressed plan is spent; a later comparison must not show it.
            plan = sync.foldPlan
        }
        announce(new)
    }

    private func announce(_ scene: FoldScene) {
        let text: String? = switch scene {
        case .plan: sync.shortStatus
        case .pressing: String(localized: "Syncing")
        case .imprint: sync.imprint.map { imprint in
            ([imprint.title] + imprint.figures.map { "\($0.value) \($0.label)" } + imprint.notes)
                .joined(separator: ", ")
        }
        case .comparing, .lifting, .message: nil
        }
        if let text { AccessibilityNotification.Announcement(text).post() }
    }

    @ViewBuilder
    private func leafLayer(lines: [FoldLine]) -> some View {
        if swing > 0, paperSize.width > 0 {
            if reduceMotion {
                // No swing: the back of the leaf fades in over the other half.
                Paper(tone: FoldPalette.paperBack)
                    .frame(width: halfWidth, height: paperSize.height)
                    .opacity(swing)
                    .offset(x: leafSide == .a ? halfWidth : 0)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            } else {
                leaf(lines: lines)
                    .frame(width: halfWidth, height: paperSize.height)
                    .rotation3DEffect(.degrees(Self.turnSign * (leafSide == .a ? 1 : -1) * swing * 180),
                                      axis: (x: 0, y: 1, z: 0),
                                      anchor: leafSide == .a ? .trailing : .leading,
                                      perspective: 0.32)
                    .offset(x: leafSide == .a ? 0 : halfWidth)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
        }
    }

    /// The lifted half: its front, then past upright its back, darkening
    /// as it turns away from the light overhead.
    private func leaf(lines: [FoldLine]) -> some View {
        let light = sin(swing * .pi)
        return ZStack {
            if swing >= FoldMotion.tippingPoint {
                Paper(tone: FoldPalette.paperBack)
                    .overlay(Color.black.opacity(0.14 * light))
                    .scaleEffect(x: -1)
            } else {
                leafFront(lines: lines)
                    .overlay(Color.black.opacity(0.2 * light))
            }
        }
        .compositingGroup()
        .shadow(color: .black.opacity(0.3 * light), radius: 12 * light)
    }

    /// The lifted half's front: its folder label and the lines the sheet
    /// showed at the scroll position, frozen while it swings.
    private func leafFront(lines: [FoldLine]) -> some View {
        let entries = FoldEntry.entries(lines)
        let first = min(entries.count, Int(scrollOffset / FoldMetrics.rowHeight))
        let visible = Int(paperSize.height / FoldMetrics.rowHeight) + 2
        let window = Array(entries.dropFirst(first).prefix(visible))
        let creaseInset = FoldMetrics.creaseWidth / 2
        return ZStack(alignment: .topLeading) {
            Paper()
            VStack(spacing: 0) {
                FoldFolderLabel(viewModel: viewModel,
                                folder: leafSide == .a ? pair.settings.left : pair.settings.right,
                                side: leafSide)
                    .frame(height: Self.headerHeight)
                VStack(spacing: 0) {
                    ForEach(window) { entry in
                        leafCell(entry)
                            .frame(height: FoldMetrics.rowHeight)
                    }
                }
                .offset(y: CGFloat(first) * FoldMetrics.rowHeight - scrollOffset + Self.listInset)
                .frame(maxHeight: .infinity, alignment: .top)
                .clipped()
            }
            .padding(leafSide == .a ? .trailing : .leading, creaseInset)
        }
    }

    @ViewBuilder
    private func leafCell(_ entry: FoldEntry) -> some View {
        switch entry {
        case .folder(let path, let count):
            FoldHeadingHalf(path: path, count: count, side: leafSide)
        case .line(let line):
            FoldHalf(line: line, side: leafSide, compact: compact)
        }
    }

    /// What the folded leaf's back says while the run presses: the
    /// run's own progress, never a claim about any one line.
    private var pressReadout: some View {
        VStack(spacing: 10) {
            Text(pair.settings.mode.title)
                .font(FoldType.imprintTitle)
                .tracking(1.5)
                .foregroundStyle(FoldPalette.pencil)
            Text(sync.progressFraction.map { $0.formatted(.percent.precision(.fractionLength(0))) }
                 ?? String(localized: "Pressing"))
                .font(FoldType.figure)
                .foregroundStyle(FoldPalette.ink(leafSide))
                .contentTransition(.numericText())
            Group {
                if let fraction = sync.progressFraction {
                    ProgressView(value: fraction)
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
            }
            .tint(FoldPalette.ink(leafSide))
            .frame(maxWidth: 220)
            .accessibilityLabel("Progress")
            Text(sync.statusText)
                .font(.callout)
                .foregroundStyle(FoldPalette.pencil)
                .multilineTextAlignment(.center)
                .lineLimit(3)
                .accessibilityAddTraits(.updatesFrequently)
            Button("Stop") { Task { await sync.cancel() } }
                .disabled(!sync.canCancel)
        }
        .padding(24)
        .frame(width: halfWidth)
        .frame(maxHeight: .infinity)
        .offset(x: leafSide == .a ? halfWidth : 0)
        .transition(.opacity)
    }

    // MARK: Desk

    /// The strip under the sheet: what the sheet says in words, and what
    /// can follow it.
    private func desk(scene: FoldScene) -> some View {
        HStack(alignment: .center, spacing: 12) {
            deskText(scene: scene)
                .frame(maxWidth: .infinity, alignment: .leading)
            deskButtons(scene: scene)
        }
        .frame(minHeight: 28)
    }

    @ViewBuilder
    private func deskText(scene: FoldScene) -> some View {
        if let turn = staged {
            Label {
                Text("\(turn.title): \(turn.name)")
                    .lineLimit(1)
                    .truncationMode(.middle)
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
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .help(sync.statusText)
                .textSelection(.enabled)
        }
    }

    @ViewBuilder
    private func deskButtons(scene: FoldScene) -> some View {
        HStack(spacing: 8) {
            if staged != nil {
                Button("Cancel") { staged = nil }
                Button(staged?.title ?? "") { _ = runStaged() }
                    .buttonStyle(.borderedProminent)
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

extension EnvironmentValues {
    /// How much slower the fold moves than it should: 1, except in a
    /// Debug tour that keeps frames of it in motion.
    @Entry var foldTimeScale: Double = 1
}

private extension FoldEntry {
    /// The change row a line entry stands for.
    var lineID: ChangeRow.ID? {
        if case .line(let line) = self { return line.id }
        return nil
    }
}

private extension FoldMotion {
    /// Where the leaf waits while the approval dialog asks: nearly
    /// upright, still visibly lifted.
    static let heldUpright: Double = 0.42
}

/// One of the pair's folders on its half of the page: where it is, its
/// full path on hover, and a menu to show it in Finder or copy its path.
struct FoldFolderLabel: View {
    let viewModel: MainViewModel
    let folder: SelectedFolder
    let side: FolderSide

    var body: some View {
        let text = viewModel.folderText(folder)
        VStack(alignment: side == .a ? .leading : .trailing, spacing: 1) {
            Text(PairSyncViewModel.folderName(side))
                .font(FoldType.meta)
                .foregroundStyle(FoldPalette.pencil)
            Label(text, systemImage: viewModel.folderSymbol(folder))
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(FoldPalette.ink(side))
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.leading, side == .a ? 30 : 8)
        .padding(.trailing, side == .a ? 8 : 30)
        .frame(maxWidth: .infinity, alignment: side == .a ? .leading : .trailing)
        .help(text)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(PairSyncViewModel.folderName(side)): \(text)"))
        .contextMenu {
            if viewModel.localURL(folder) != nil {
                Button("Show in Finder") { viewModel.reveal(side) }
            }
            Button("Copy Path") {
                let pasteboard = NSPasteboard.general
                pasteboard.clearContents()
                pasteboard.setString(PairEditorViewModel.pathText(folder), forType: .string)
            }
        }
    }
}

/// The run's imprint, set across the crease: its title, its counts in
/// large figures, and what it left for a person.
struct FoldImprintView: View {
    let imprint: FoldImprint
    let ink: Color

    var body: some View {
        VStack(spacing: 8) {
            Text(imprint.title)
                .font(FoldType.imprintTitle)
                .tracking(2)
                .foregroundStyle(FoldPalette.pencil)
            HStack(alignment: .firstTextBaseline, spacing: 22) {
                ForEach(imprint.figures, id: \.label) { figure in
                    VStack(spacing: 0) {
                        Text(figure.value)
                            .font(FoldType.figure)
                        Text(figure.label)
                            .font(FoldType.meta)
                    }
                }
            }
            .foregroundStyle(ink)
            // Letterpress: the figures sit a hair below the paper.
            .shadow(color: .white.opacity(0.6), radius: 0, x: 0, y: 1)
            ForEach(imprint.notes, id: \.self) { note in
                Text(note)
                    .font(.callout)
                    .foregroundStyle(FoldPalette.pencil)
            }
        }
        .padding(.vertical, 18)
        .padding(.horizontal, 30)
        .overlay {
            RoundedRectangle(cornerRadius: 2)
                .strokeBorder(ink.opacity(0.3), lineWidth: 1.25)
                .padding(3)
        }
        .accessibilityElement(children: .combine)
    }
}

/// A soft band of light passing down the page while the folders are
/// compared; still under Reduce Motion.
private struct ComparingSweep: View {
    let paused: Bool
    private static let period = 1.6
    private static let band: CGFloat = 80

    var body: some View {
        if !paused {
            TimelineView(.animation) { context in
                let phase = context.date.timeIntervalSinceReferenceDate
                    .truncatingRemainder(dividingBy: Self.period) / Self.period
                GeometryReader { proxy in
                    LinearGradient(colors: [.clear, Color.accentColor.opacity(0.10), .clear],
                                   startPoint: .top, endPoint: .bottom)
                        .frame(height: Self.band)
                        .offset(y: phase * (proxy.size.height + Self.band) - Self.band)
                }
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
    }
}

/// A short side-to-side shake: the line will not turn that way.
private struct Shake: GeometryEffect {
    /// The shake count; each step from one count to the next swings
    /// twice and comes to rest.
    var animatableData: Double

    init(count: Int) {
        animatableData = Double(count)
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(translationX: 2.5 * sin(animatableData * .pi * 4), y: 0))
    }
}
