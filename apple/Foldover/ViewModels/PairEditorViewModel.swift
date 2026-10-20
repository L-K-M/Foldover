import Foundation
import Observation

/// The locator prefix `PairRoot` uses to detect an SFTP folder.
private let sftpScheme = "sftp://"

enum PairFolderSide: Hashable {
    case left
    case right
}

@Observable
@MainActor
final class PairEditorViewModel: Identifiable {
    let id = UUID()
    private let store: PairStore
    /// Paired Macs and their folders; nil hides the choice.
    private let macs: MacToMacStore?
    private let editing: SyncPair?
    var name: String
    var mode: PairMode
    var options: PairOptions {
        didSet {
            if options.syncModificationTimes != oldValue.syncModificationTimes
                || options.syncExtendedAttributes != oldValue.syncExtendedAttributes
                || options.syncPermissions != oldValue.syncPermissions {
                queueReadinessCheck()
            }
        }
    }
    private(set) var left: SelectedFolder?
    private(set) var right: SelectedFolder?
    private(set) var isSaving = false
    private(set) var errorMessage: String?
    private(set) var savedPairID: String?
    /// What the agent says will not work as expected with the picked
    /// folders (PLAN.md §3.6); warnings never block saving.
    private(set) var readinessWarnings: [String] = []
    /// SMB servers found on the local network (PLAN.md §3.8); empty
    /// until the browse answers.
    private(set) var nearbyServers: [NearbyServer] = []
    /// The SFTP folder being typed or browsed to (PLAN.md §4.3).
    var serverLocator = ""
    /// The folders in `serverLocator`'s folder once browsed.
    private(set) var serverListing: ServerListing?
    private(set) var isBrowsingServer = false
    private(set) var serverBrowseError: String?
    private var checkTask: Task<Void, Never>?
    private var checkGeneration = 0
    private var serversTask: Task<Void, Never>?
    /// Only the latest browse answers, when a person clicks faster than
    /// the server replies.
    private var browseGeneration = 0

    init(store: PairStore, editing: SyncPair? = nil, macs: MacToMacStore? = nil) {
        self.store = store
        self.macs = macs
        self.editing = editing
        name = editing?.settings.name ?? ""
        mode = editing?.settings.mode ?? .sync
        options = editing?.settings.options ?? .defaults
        left = editing?.settings.left
        right = editing?.settings.right
        queueReadinessCheck()
        serversTask = Task { await refreshNearbyServers() }
    }

    var title: String { editing == nil ? String(localized: "New Pair") : String(localized: "Edit Pair") }
    var canSave: Bool { !isSaving && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && left != nil && right != nil && durabilityWarning == nil && canaryWarning == nil && dailyWarning == nil }

    /// Exclusion rules as editable text, one per line.
    var filterText: String {
        get { options.filters.joined(separator: "\n") }
        set {
            options.filters = newValue.split(separator: "\n", omittingEmptySubsequences: false)
                .map(String.init)
        }
    }

    /// Sizes in whole decimal megabytes, as Finder shows them.
    private static let bytesPerMegabyte: UInt64 = 1_000_000
    private static let suggestedLimitMegabytes: UInt64 = 1_000

    var limitsSize: Bool {
        get { options.sizeLimit != nil }
        set {
            options.sizeLimit = newValue
                ? options.sizeLimit ?? Self.suggestedLimitMegabytes * Self.bytesPerMegabyte
                : nil
        }
    }

    var sizeLimitMegabytes: Int {
        get { Int(clamping: (options.sizeLimit ?? 0) / Self.bytesPerMegabyte) }
        set {
            let megabytes = min(UInt64(max(1, newValue)), UInt64.max / Self.bytesPerMegabyte)
            options.sizeLimit = megabytes * Self.bytesPerMegabyte
        }
    }

    private static let suggestedAgeDays: UInt32 = 365
    private static let maxAgeDays: UInt32 = 36_500

    var limitsAge: Bool {
        get { options.ageLimit != nil }
        set { options.ageLimit = newValue ? options.ageLimit ?? Self.suggestedAgeDays : nil }
    }

    var ageLimitDays: Int {
        get { Int(options.ageLimit ?? 0) }
        set { options.ageLimit = UInt32(min(max(1, newValue), Int(Self.maxAgeDays))) }
    }

    /// The file the run checks before it writes, as editable text. Empty
    /// means none; surrounding whitespace and a leading "/" are forgiven.
    var canary: String {
        get { options.canary.map { String(decoding: $0, as: UTF8.self) } ?? "" }
        set {
            var text = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if text.hasPrefix("/") { text.removeFirst() }
            options.canary = text.isEmpty ? nil : Data(text.utf8)
        }
    }

    /// A canary names a file inside the synced folders, never inside
    /// `.foldover`, the pair's own state folder.
    var canaryWarning: String? {
        guard let canary = options.canary else { return nil }
        let components = canary.split(separator: UInt8(ascii: "/"), omittingEmptySubsequences: false)
        let reserved = components.first == Data(".foldover".utf8)
        let malformed = components.contains { $0.isEmpty || $0 == Data(".".utf8) || $0 == Data("..".utf8) }
        guard reserved || malformed else { return nil }
        return String(localized: "Name a file inside the synced folders, like docs/canary.txt; the .foldover folder is not allowed.")
    }

    /// The daily schedule a new Daily choice starts with: 02:00, all days.
    static let defaultDaily: SyncSchedule = .daily(minutes: 2 * 60, days: Set(SyncDay.allCases))

    /// Fixed choices, plus a stored interval set elsewhere (e.g. the CLI).
    /// The Daily row carries the current daily settings, so it stays the
    /// picker's selection while the time and days are edited.
    var scheduleChoices: [SyncSchedule] {
        let daily = if case .daily = options.schedule { options.schedule } else { Self.defaultDaily }
        var choices: [SyncSchedule] = [.manual, .onChange, .every(minutes: 5), .every(minutes: 15),
                                       .every(minutes: 60), .every(minutes: 24 * 60), daily]
        if !choices.contains(options.schedule) { choices.append(options.schedule) }
        return choices
    }

    /// How quickly "When files change" notices changes (PLAN.md §4.8):
    /// this Mac's own at once, other computers' on shares and servers by
    /// looking every few minutes.
    var scheduleNote: String? {
        guard options.schedule == .onChange else { return nil }
        return String(localized: "Syncs 10 seconds after changes stop, or after 5 minutes of nonstop changes. Shares and SFTP servers are checked for other computers' changes every 5 minutes, and both folders are compared in full every hour.")
    }

    /// The Daily time and weekday controls are shown only for Daily.
    var showsDailySettings: Bool {
        if case .daily = options.schedule { true } else { false }
    }

    /// Minutes after local midnight the daily schedule runs.
    var dailyMinutes: Int {
        get {
            if case .daily(let minutes, _) = options.schedule { Int(minutes) } else { 2 * 60 }
        }
        set {
            if case .daily(_, let days) = options.schedule {
                options.schedule = .daily(minutes: UInt32(min(1439, max(0, newValue))), days: days)
            }
        }
    }

    /// `dailyMinutes` as a `Date` on a fixed day, for the time picker.
    var dailyTime: Date {
        get {
            Calendar.current.date(
                bySettingHour: dailyMinutes / 60, minute: dailyMinutes % 60, second: 0,
                of: Date(timeIntervalSince1970: 0)
            ) ?? Date(timeIntervalSince1970: 0)
        }
        set {
            let parts = Calendar.current.dateComponents([.hour, .minute], from: newValue)
            dailyMinutes = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        }
    }

    /// The weekdays the daily schedule runs on.
    var dailyDays: Set<SyncDay> {
        get {
            if case .daily(_, let days) = options.schedule { days } else { Set(SyncDay.allCases) }
        }
        set {
            if case .daily(let minutes, _) = options.schedule {
                options.schedule = .daily(minutes: minutes, days: newValue)
            }
        }
    }

    static func scheduleTitle(_ schedule: SyncSchedule) -> String {
        switch schedule {
        case .manual: String(localized: "Only when I click Sync Now")
        case .onChange: String(localized: "When files change")
        case .every(60): String(localized: "Every hour")
        case .every(1440): String(localized: "Every day")
        case .every(let minutes): String(localized: "Every \(minutes) minutes")
        case .daily(let minutes, let days): dailyTitle(minutes: Int(minutes), days: days)
        }
    }

    /// Monday through Friday, the daily schedule titled "Weekdays".
    private static let weekdays: Set<SyncDay> = [.monday, .tuesday, .wednesday, .thursday, .friday]

    private static func timeText(_ minutes: Int) -> String {
        String(format: "%02d:%02d", minutes / 60, minutes % 60)
    }

    /// "Every day at 02:30", "Weekdays at 18:00" or "Mon, Wed, Fri at 07:15".
    private static func dailyTitle(minutes: Int, days: Set<SyncDay>) -> String {
        let time = timeText(minutes)
        if days.count == SyncDay.allCases.count {
            return String(localized: "Every day at \(time)")
        }
        if days == weekdays {
            return String(localized: "Weekdays at \(time)")
        }
        let list = SyncDay.allCases.filter(days.contains).map(\.shortName).joined(separator: ", ")
        return String(localized: "\(list) at \(time)")
    }

    /// What the pair will do, in at most three short sentences that
    /// rewrite themselves as the settings change (PLAN.md §3.8): the
    /// direction, what happens to deleted files plus any unusual
    /// choices, and when syncs run.
    var planText: String {
        let details = planDetails
        var consequence = [Self.deletionClause(mode)]
        consequence.append(contentsOf: details)
        let joined = consequence.joined(separator: "; ")
        let consequenceSentence = "\(joined.prefix(1).uppercased())\(joined.dropFirst())."
        return [Self.directionSentence(mode), consequenceSentence, scheduleSentence]
            .joined(separator: " ")
    }

    private static func directionSentence(_ mode: PairMode) -> String {
        switch mode {
        case .sync: String(localized: "Changes in either folder go to the other.")
        case .mirrorLeft: String(localized: "Folder B is made identical to A.")
        case .mirrorRight: String(localized: "Folder A is made identical to B.")
        case .backupLeft: String(localized: "Folder A is copied to Folder B.")
        case .backupRight: String(localized: "Folder B is copied to Folder A.")
        }
    }

    private static func deletionClause(_ mode: PairMode) -> String {
        switch mode {
        case .sync: String(localized: "a file deleted in one folder is archived in the other")
        case .mirrorLeft: String(localized: "files you delete in A are archived in B, never deleted")
        case .mirrorRight: String(localized: "files you delete in B are archived in A, never deleted")
        case .backupLeft: String(localized: "files you delete in A stay in B")
        case .backupRight: String(localized: "files you delete in B stay in A")
        }
    }

    /// When syncs run, from the schedule's own title; daily titles that
    /// name weekdays keep their capitals.
    private var scheduleSentence: String {
        switch options.schedule {
        case .manual:
            return String(localized: "Syncs only when you click Sync Now.")
        case .daily(let minutes, let days) where days.isEmpty:
            return String(localized: "Syncs at \(Self.timeText(Int(minutes))) once you choose weekdays.")
        case .daily(_, let days) where days.count != SyncDay.allCases.count && days != Self.weekdays:
            return String(localized: "Syncs \(Self.scheduleTitle(options.schedule)).")
        default:
            let title = Self.scheduleTitle(options.schedule)
            return String(localized: "Syncs \(title.prefix(1).lowercased())\(title.dropFirst()).")
        }
    }

    /// Metadata choices that differ from the engine's defaults, the skip
    /// rules and limits, and the canary, as clauses in the plan's second
    /// sentence.
    private var planDetails: [String] {
        let defaults = PairOptions.defaults
        var clauses: [String] = []
        if options.syncPermissions != defaults.syncPermissions {
            clauses.append(options.syncPermissions
                ? String(localized: "permissions and access lists sync")
                : String(localized: "permissions and access lists don't sync"))
        }
        if options.syncExtendedAttributes != defaults.syncExtendedAttributes {
            clauses.append(options.syncExtendedAttributes
                ? String(localized: "tags and other extended attributes sync")
                : String(localized: "tags and other extended attributes don't sync"))
        }
        if options.syncModificationTimes != defaults.syncModificationTimes {
            clauses.append(options.syncModificationTimes
                ? String(localized: "modification dates sync")
                : String(localized: "modification dates don't sync"))
        }
        var skipped: [String] = []
        let rules = options.filters.filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
        if !rules.isEmpty {
            skipped.append(String(localized: "\(rules.count) rules"))
        }
        if let sizeLimit = options.sizeLimit {
            skipped.append(String(localized: "files larger than \(PairSyncViewModel.byteText(sizeLimit))"))
        }
        if let ageLimit = options.ageLimit {
            skipped.append(String(localized: "files unchanged for more than \(ageLimit) days"))
        }
        if !skipped.isEmpty {
            clauses.append(String(localized: "skipped: \(skipped.joined(separator: ", "))"))
        }
        if let canary = options.canary {
            clauses.append(String(localized: "the canary file \(String(decoding: canary, as: UTF8.self)) must stay unchanged in both folders"))
        }
        return clauses
    }

    /// A daily schedule needs at least one weekday; the engine rejects an
    /// empty set, so saving is blocked instead.
    var dailyWarning: String? {
        if case .daily(_, let days) = options.schedule, days.isEmpty {
            String(localized: "Choose at least one weekday.")
        } else {
            nil
        }
    }

    /// Versions depend on synced metadata, so changing it resets sync history.
    var resetsSyncHistory: Bool {
        guard let editing else { return false }
        return !options.syncsSameMetadata(as: editing.settings.options)
    }

    /// Either side is a folder on an SFTP server.
    var usesServerFolder: Bool {
        left?.isServerFolder == true || right?.isServerFolder == true
    }

    /// Standard SFTP cannot atomically confine server-side link changes.
    var serverWarning: String? {
        usesServerFolder ? String(localized: "Use SFTP only with trusted servers. Folder or link changes there can redirect access outside your chosen folder.") : nil
    }

    /// What metadata each side keeps. A server carries file contents,
    /// permissions and whole-second dates, not the rest (ADR 0028).
    var metadataNote: String {
        if usesOtherMac {
            String(localized: "Copies between Macs keep tags, Finder info and other extended attributes, resource forks and file flags. Access control lists don't sync between Macs yet.")
        } else if usesServerFolder {
            String(localized: "SFTP keeps file data, mode bits and whole-second modification times. Extended attributes and ACLs are omitted; resource-fork files cannot be copied.")
        } else {
            String(localized: "Resource forks always sync. Without permissions, a file's executable bit still syncs.")
        }
    }

    /// Power-loss-safe writes need the local endpoint's rename and
    /// drive-flush contract; SFTP offers only per-file fsync (ADR 0028).
    /// A stored "Survive power loss" keeps its picker tag until the
    /// user chooses Standard, so the invalid selection stays visible.
    var durabilityChoices: [PairDurability] {
        usesServerFolder && options.durability == .standard ? [.standard] : PairDurability.allCases
    }

    /// A stored "Survive power loss" choice conflicts with a server
    /// folder; the user must choose Standard before the pair can save.
    var durabilityWarning: String? {
        guard usesServerFolder, options.durability == .full else { return nil }
        return String(localized: "Survive power loss isn't available for SFTP: syncing a file can't promise the server's drive cache is flushed. Choose Standard.")
    }

    /// On a server, writes follow its acknowledged-write policy.
    var durabilityExplanation: String {
        if usesServerFolder {
            String(localized: "Writes follow the server's own durability; SFTP can't make it flush its drive cache.")
        } else {
            options.durability.explanation
        }
    }

    func folderText(_ side: PairFolderSide) -> String {
        guard let folder = side == .left ? left : right else {
            return String(localized: "Choose a folder")
        }
        return macs?.folderName(for: folder.bytes) ?? Self.pathText(folder)
    }

    /// Either side is a folder a paired Mac shares.
    var usesOtherMac: Bool { [left, right].contains { $0?.isPeerFolder == true } }

    var offersOtherMacs: Bool { macs != nil }

    var pairedMacs: [PairedDevice] { macs?.state?.paired ?? [] }

    var otherMacsNote: String {
        pairedMacs.isEmpty
            ? String(localized: "Pair a Mac in Settings › Mac-to-Mac first, then share a folder on it with this Mac.")
            : String(localized: "Only folders that Mac shares with this one are listed.")
    }

    /// Why a paired Mac's folders could not be listed, if they could not.
    var otherMacsError: String? { macs?.error }

    func folders(on mac: PairedDevice) async -> [OfferedShare] {
        await macs?.offered(by: mac.device) ?? []
    }

    /// Use `share` on `mac` as this side's folder.
    func chooseShared(_ share: OfferedShare, on mac: PairedDevice, side: PairFolderSide) {
        let folder = SelectedFolder(bytes: Data("\(peerScheme)\(mac.device)/\(share.id)".utf8))
        switch side {
        case .left: left = folder
        case .right: right = folder
        }
        errorMessage = nil
        queueReadinessCheck()
    }

    func chooseFolder(_ result: Result<[URL], Error>, side: PairFolderSide) {
        do {
            guard let url = try result.get().first else { return }
            let folder = try store.folder(from: url)
            switch side {
            case .left: left = folder
            case .right: right = folder
            }
            errorMessage = nil
            queueReadinessCheck()
        } catch {
            errorMessage = PairStore.message(for: error)
        }
    }

    /// Use a folder on an SFTP server, `sftp://[user@]host[:port]/path`.
    /// The agent checks the rest and connects when the pair is saved.
    func chooseServer(_ locator: String, side: PairFolderSide) {
        let locator = locator.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Self.isServerLocator(locator) else {
            errorMessage = Self.serverLocatorHelp
            return
        }
        let folder = SelectedFolder(bytes: Data(locator.utf8))
        switch side {
        case .left: left = folder
        case .right: right = folder
        }
        errorMessage = nil
        queueReadinessCheck()
    }

    /// List the folders in `serverLocator`'s folder on its server, to
    /// walk in and out of before choosing one.
    func browseServer() async {
        let locator = serverLocator.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Self.isServerLocator(locator) else {
            serverBrowseError = Self.serverLocatorHelp
            return
        }
        browseGeneration += 1
        let generation = browseGeneration
        isBrowsingServer = true
        serverBrowseError = nil
        do {
            let listing = try await store.browseServer(locator)
            guard generation == browseGeneration else { return }
            serverListing = listing
        } catch {
            guard generation == browseGeneration else { return }
            serverBrowseError = PairStore.message(for: error)
        }
        isBrowsingServer = false
    }

    /// Use `serverLocator` for `side`; false, with the reason shown, when
    /// it does not name a folder on a server.
    func useServerFolder(side: PairFolderSide) -> Bool {
        let locator = serverLocator.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Self.isServerLocator(locator) else {
            serverBrowseError = Self.serverLocatorHelp
            return false
        }
        chooseServer(locator, side: side)
        return true
    }

    /// Browse into a listed folder, or up to the parent.
    func openServerFolder(_ locator: String) async {
        serverLocator = locator
        await browseServer()
    }

    /// Start over for a new choice of server folder.
    func resetServerBrowser() {
        browseGeneration += 1
        serverLocator = ""
        serverListing = nil
        isBrowsingServer = false
        serverBrowseError = nil
    }

    private static func isServerLocator(_ locator: String) -> Bool {
        locator.hasPrefix(sftpScheme) && locator.dropFirst(sftpScheme.count).contains("/")
    }

    private static var serverLocatorHelp: String {
        String(localized: "Write the folder as sftp://host/path, for example sftp://nas.local/~/Photos.")
    }

    /// Look for file servers on the local network; the browse waits
    /// about two seconds. A failed one keeps the last answer: the list
    /// is advisory, and the SFTP path stays available regardless.
    func refreshNearbyServers() async {
        guard let servers = try? await store.nearbyServers() else { return }
        nearbyServers = servers
    }

    /// Wait for the browse the editor queued; tests use it.
    func awaitNearbyServers() async {
        await serversTask?.value
    }

    /// SMB's well-known port; Finder's `smb://` URLs omit it.
    private static let smbDefaultPort: UInt16 = 445

    /// Where Connect takes `server` in Finder: `smb://host`, with the
    /// port only when it is not the SMB default.
    func serverURL(_ server: NearbyServer) -> URL? {
        if server.port == Self.smbDefaultPort {
            return URL(string: "smb://\(server.host)")
        }
        return URL(string: "smb://\(server.host):\(server.port)")
    }

    /// Ask the agent what will not work as expected with the picked
    /// folders. A failed check keeps the last answer: it is advisory,
    /// and saving reports any real problem itself.
    func refreshReadiness() async {
        checkGeneration += 1
        let generation = checkGeneration
        guard let left, let right else {
            readinessWarnings = []
            return
        }
        let settings = PairSettings(name: name, left: left, right: right, mode: mode, options: options)
        guard let warnings = try? await store.check(settings), generation == checkGeneration else { return }
        readinessWarnings = warnings.map(text)
    }

    private func queueReadinessCheck() {
        checkTask = Task { await refreshReadiness() }
    }

    /// Wait for a queued check; tests use it after changing a side or a
    /// metadata choice.
    func awaitReadinessCheck() async {
        await checkTask?.value
    }

    private func text(for warning: PairReadinessWarning) -> String {
        let side = warning.side
        let other: FolderSide = side == .a ? .b : .a
        switch warning.kind {
        case .cloudManaged(let provider):
            return String(localized: "\(PairSyncViewModel.folderName(side)) is in \(provider), which also syncs it. Two sync engines on one folder can undo each other's changes; prefer a folder outside it.")
        case .metadataUnsupported where (side == .a ? left : right)?.isServerFolder == true:
            return String(localized: "\(PairSyncViewModel.folderName(side)) is on an SFTP server, which keeps no tags or other extended attributes; they will not sync to it.")
        case .metadataUnsupported:
            return String(localized: "\(PairSyncViewModel.folderName(side)) cannot keep tags or other extended attributes; they will not sync to it.")
        case .namesDiffer:
            return String(localized: "\(PairSyncViewModel.folderName(side)) ignores the case of names and \(PairSyncViewModel.folderName(other)) does not: names that differ only in case become conflicts to rename.")
        }
    }

    func save() async -> Bool {
        guard canSave, let left, let right else { return false }
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }
        do {
            let saved = try await store.save(PairSettings(
                name: name, left: left, right: right, mode: mode, options: options
            ), editing: editing)
            savedPairID = saved.id
            return true
        } catch {
            errorMessage = PairStore.message(for: error)
            return false
        }
    }

    static func pathText(_ folder: SelectedFolder) -> String {
        String(data: folder.bytes, encoding: .utf8) ?? String(localized: "Path contains non-UTF-8 bytes")
    }
}

extension SelectedFolder {
    /// A folder a paired Mac shares, kept as a `peer://…` locator.
    var isPeerFolder: Bool {
        bytes.starts(with: Data(peerScheme.utf8))
    }

    /// A folder on an SFTP server, kept as an `sftp://…` locator;
    /// the same byte test `PairRoot` applies.
    var isServerFolder: Bool {
        bytes.starts(with: Data(sftpScheme.utf8))
    }
}

extension PairMode {
    var title: String {
        switch self {
        case .sync: String(localized: "Sync both ways")
        case .mirrorLeft: String(localized: "Mirror A to B")
        case .mirrorRight: String(localized: "Mirror B to A")
        case .backupLeft: String(localized: "Back up A to B")
        case .backupRight: String(localized: "Back up B to A")
        }
    }

    var explanation: String {
        switch self {
        case .sync: String(localized: "Changes flow both ways. Conflicting edits wait for your decision.")
        case .mirrorLeft: String(localized: "Folder B is made identical to A. Replaced and removed files are archived for 30 days.")
        case .mirrorRight: String(localized: "Folder A is made identical to B. Replaced and removed files are archived for 30 days.")
        case .backupLeft: String(localized: "New and changed files are copied from A to B. Nothing in B is deleted.")
        case .backupRight: String(localized: "New and changed files are copied from B to A. Nothing in A is deleted.")
        }
    }
}

extension SyncDay {
    var shortName: String {
        switch self {
        case .monday: String(localized: "Mon")
        case .tuesday: String(localized: "Tue")
        case .wednesday: String(localized: "Wed")
        case .thursday: String(localized: "Thu")
        case .friday: String(localized: "Fri")
        case .saturday: String(localized: "Sat")
        case .sunday: String(localized: "Sun")
        }
    }
}

extension PairDurability {
    var title: String {
        switch self {
        case .standard: String(localized: "Standard")
        case .full: String(localized: "Survive power loss")
        }
    }

    var explanation: String {
        switch self {
        case .standard: String(localized: "Written files survive app and system crashes.")
        case .full: String(localized: "Also flushes the drive's cache. Slower: about 10–30 ms more per file.")
        }
    }
}
