import Foundation

/// Folder rows carry the same operations as files (PLAN.md §3.8).
enum PlanAction: String, Codable, Sendable {
    case copyToTarget
    case copyToMac
    case archive
    case conflict

    var glyph: String {
        switch self {
        case .copyToTarget: "→"
        case .copyToMac: "←"
        case .archive: "⌫"
        case .conflict: "⚠"
        }
    }

    var accessibilityVerb: String {
        switch self {
        case .copyToTarget: "copy to MyNAS"
        case .copyToMac: "copy to this Mac"
        case .archive: "archive on MyNAS"
        case .conflict: "edited on both sides"
        }
    }
}

/// Pre-order delivery guarantees that `parentID` already landed; −1 is top level.
struct RowRecord: Equatable, Sendable {
    var id: Int
    var parentID: Int
    var name: String
    var isFolder: Bool
    var action: PlanAction
    var sizeBytes: Int64
    /// Days before the fixed display date, without a Date allocation per record.
    var daysAgo: Int
}

struct PreparedRow: Sendable {
    let record: RowRecord
    let sizeText: String
    let dateText: String
}

struct PreparedBatch: Sendable {
    let rows: [PreparedRow]
    let sizeFormattingMilliseconds: Double
    let dateFormattingMilliseconds: Double
    let formattedSizes: Int
    let formattedDates: Int
    let preparedOnMainThread: Bool
}

/// Keep both formatters on one worker; send only values to the main actor.
actor RowText {
    private let dateFormatter = DateFormatter()
    private let byteFormatter = ByteCountFormatter()
    private let baseDate = Date(timeIntervalSince1970: 1_790_985_600)
    private let clock = ContinuousClock()

    init() {
        dateFormatter.dateFormat = "MMM d"
        byteFormatter.countStyle = .file
    }

    func prepare(_ records: ArraySlice<RowRecord>) -> PreparedBatch {
        var rows: [PreparedRow] = []
        rows.reserveCapacity(records.count)
        var sizeTime = Duration.zero
        var dateTime = Duration.zero
        var formattedSizes = 0
        var formattedDates = 0

        for record in records {
            let sizeStarted = clock.now
            let size = byteFormatter.string(fromByteCount: record.sizeBytes)
            sizeTime += clock.now - sizeStarted
            formattedSizes += 1

            let dateStarted = clock.now
            let date = dateFormatter.string(
                from: baseDate.addingTimeInterval(-Double(record.daysAgo) * 86_400)
            )
            dateTime += clock.now - dateStarted
            formattedDates += 1

            rows.append(PreparedRow(record: record, sizeText: size, dateText: date))
        }

        return PreparedBatch(
            rows: rows,
            sizeFormattingMilliseconds: sizeTime / .milliseconds(1),
            dateFormattingMilliseconds: dateTime / .milliseconds(1),
            formattedSizes: formattedSizes,
            formattedDates: formattedDates,
            preparedOnMainThread: Thread.isMainThread
        )
    }
}
