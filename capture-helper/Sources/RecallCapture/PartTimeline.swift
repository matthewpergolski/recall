import CoreAudio
import Foundation

/// Records when each audio part started, on the machine's host clock.
///
/// The mic and the call recorder are separate processes, and a track can roll
/// into a new part. With one start time per part, Recall can place both tracks
/// on one timeline at transcription. The record holds times only, no audio.
enum PartTimeline {
    struct Record: Codable, Equatable {
        let file: String
        let source: String
        /// Host time of the part's first buffer, in nanoseconds since boot.
        let hostTimeNs: UInt64
        /// The same moment as Unix milliseconds, for reading by a person.
        let unixMs: Int64

        enum CodingKeys: String, CodingKey {
            case file, source
            case hostTimeNs = "host_time_ns"
            case unixMs = "unix_ms"
        }
    }

    /// Host time of a Core Audio buffer, or nil when the device gave none.
    static func hostTimeNs(_ timeStamp: UnsafePointer<AudioTimeStamp>?) -> UInt64? {
        guard let stamp = timeStamp?.pointee,
              stamp.mFlags.contains(.hostTimeValid),
              stamp.mHostTime != 0
        else {
            return nil
        }
        return AudioConvertHostTimeToNanos(stamp.mHostTime)
    }

    /// `<session>/audio/<name>` is described by `<session>/.recall/timeline/<name>.json`.
    static func recordURL(forAudio audioURL: URL) -> URL {
        audioURL.deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent(".recall", isDirectory: true)
            .appendingPathComponent("timeline", isDirectory: true)
            .appendingPathComponent(audioURL.lastPathComponent + ".json")
    }

    /// Writes the start time of a part. With no host time, an older record of
    /// the same name is removed, so a stale time is never read as this part's.
    /// Never throws: a missing record costs alignment, not the recording.
    static func record(audioURL: URL, source: String, hostTimeNs: UInt64?) {
        let url = recordURL(forAudio: audioURL)
        guard let hostTimeNs else {
            try? FileManager.default.removeItem(at: url)
            return
        }
        let nowNs = AudioConvertHostTimeToNanos(AudioGetCurrentHostTime())
        let agoMs = Int64(nowNs > hostTimeNs ? (nowNs - hostTimeNs) / 1_000_000 : 0)
        let record = Record(
            file: audioURL.lastPathComponent,
            source: source,
            hostTimeNs: hostTimeNs,
            unixMs: Int64(Date().timeIntervalSince1970 * 1000) - agoMs
        )
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try JSONEncoder().encode(record).write(to: url, options: .atomic)
        } catch {
            fputs("timeline: could not write \(url.lastPathComponent): \(error)\n", stderr)
        }
    }
}

/// Watches the host-clock stamps of consecutive buffers for lost time.
///
/// A device can stop delivering audio for a moment with no format change, for
/// example while a phone call is set up. Nothing else reports that; the stamp
/// of the next buffer is simply later than the buffer before it ended.
struct BufferClock {
    /// A buffer this much later than expected means time was lost.
    static let jumpThresholdNs: UInt64 = 100_000_000
    private var expectedNs: UInt64?

    /// Call once per buffer, in order. Returns the time lost before this
    /// buffer when it is over the threshold, and nil otherwise.
    mutating func advance(hostTimeNs: UInt64?, frames: UInt32, sampleRate: Double) -> UInt64? {
        guard let hostTimeNs, sampleRate > 0 else {
            expectedNs = nil
            return nil
        }
        defer { expectedNs = hostTimeNs + UInt64(Double(frames) / sampleRate * 1_000_000_000) }
        guard let expectedNs, hostTimeNs > expectedNs else {
            return nil
        }
        let lostNs = hostTimeNs - expectedNs
        return lostNs > Self.jumpThresholdNs ? lostNs : nil
    }
}
