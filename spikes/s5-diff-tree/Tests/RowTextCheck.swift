import Foundation

@main
private struct RowTextCheck {
    @MainActor
    static func main() async {
        let text = RowText()
        let dates = DateFormatter()
        dates.dateFormat = "MMM d"
        let sizes = ByteCountFormatter()
        sizes.countStyle = .file
        let baseDate = Date(timeIntervalSince1970: 1_790_985_600)
        let byteCounts: [Int64] = [0, 1, 1_023, 1_024, 1_025, 1_000_000, 1_000_000_000, .max]
        let dayOffsets = [-3, 0, 1, 31, 365, 366, 1_000]

        let records = byteCounts.flatMap { bytes in
            dayOffsets.map { days in
                RowRecord(
                    id: days, parentID: -1, name: "\(bytes)-\(days)",
                    isFolder: bytes == 0, action: .copyToTarget,
                    sizeBytes: bytes, daysAgo: days
                )
            }
        }
        // Nonzero slice indexes must not reorder or drop a batch's rows.
        let slice = records.dropFirst(2)
        let prepared = await text.prepare(slice)
        precondition(prepared.rows.map(\.record) == Array(slice))
        precondition(!prepared.preparedOnMainThread)
        for row in prepared.rows {
            precondition(row.sizeText == sizes.string(fromByteCount: row.record.sizeBytes))
            let date = baseDate.addingTimeInterval(-Double(row.record.daysAgo) * 86_400)
            precondition(row.dateText == dates.string(from: date))
        }
        precondition(prepared.formattedSizes == slice.count && prepared.formattedDates == slice.count)
        let empty = await text.prepare([])
        precondition(empty.rows.isEmpty && empty.formattedSizes == 0 && empty.formattedDates == 0)
        precondition(empty.sizeFormattingMilliseconds == 0 && empty.dateFormattingMilliseconds == 0)
        let repeated = await text.prepare(slice)
        precondition(repeated.rows.map(\.sizeText) == prepared.rows.map(\.sizeText))
        precondition(repeated.rows.map(\.dateText) == prepared.rows.map(\.dateText))
        precondition(repeated.formattedSizes == slice.count && repeated.formattedDates == slice.count)
        print("Worker formatting preserves row order, values and batch profiles")
    }
}
