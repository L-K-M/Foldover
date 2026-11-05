import Foundation

/// The disk or share a folder is on, as macOS reports it.
public struct VolumeReading: Equatable, Sendable {
    /// The name Finder shows, e.g. "Backup SSD".
    public let name: String
    public let totalBytes: UInt64
    /// What a person can still store there, purgeable space included
    /// where the volume says how much.
    public let availableBytes: UInt64

    public init(name: String, totalBytes: UInt64, availableBytes: UInt64) {
        self.name = name
        self.totalBytes = totalBytes
        self.availableBytes = min(availableBytes, totalBytes)
    }
}

/// Reads which volume holds a folder and how full it is.
public enum VolumeReader {
    /// A network share's file system calls can block for as long as its
    /// server stays silent. Reads run here, so a stalled share holds a
    /// thread of this queue, never the main thread or a thread of Swift's
    /// cooperative pool; callers keep one read per folder in flight.
    private static let queue = DispatchQueue(label: "ch.lkmc.foldover.volumes", qos: .utility,
                                             attributes: .concurrent)

    /// `url`'s volume; nil when macOS cannot say, as for a folder that
    /// is gone or a volume that reports no size.
    public static func read(_ url: URL) async -> VolumeReading? {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume(returning: readNow(url)) }
        }
    }

    private static func readNow(_ url: URL) -> VolumeReading? {
        let keys: Set<URLResourceKey> = [
            .volumeLocalizedNameKey, .volumeTotalCapacityKey,
            .volumeAvailableCapacityForImportantUsageKey, .volumeAvailableCapacityKey,
        ]
        // A URL keeps resource values it has read; free space moves.
        var fresh = url
        fresh.removeAllCachedResourceValues()
        guard let values = try? fresh.resourceValues(forKeys: keys),
              let total = values.volumeTotalCapacity, total > 0,
              let name = values.volumeLocalizedName,
              let available = available(important: values.volumeAvailableCapacityForImportantUsage,
                                        plain: values.volumeAvailableCapacity) else { return nil }
        return VolumeReading(name: name, totalBytes: UInt64(total), availableBytes: available)
    }

    /// The free space to show. The figure for important use counts
    /// purgeable space too, so it is the larger where a volume weighs it;
    /// a volume that does not, such as a second APFS volume or a share,
    /// reports it as 0 or not at all, and the plain figure stands.
    static func available(important: Int64?, plain: Int?) -> UInt64? {
        guard important != nil || plain != nil else { return nil }
        return max(UInt64(clamping: important ?? 0), UInt64(clamping: plain ?? 0))
    }
}
