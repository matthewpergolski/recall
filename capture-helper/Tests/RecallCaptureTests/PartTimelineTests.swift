import AVFoundation
import XCTest
@testable import RecallCapture

final class PartTimelineTests: XCTestCase {
    private func buffer(_ channels: UInt32) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: channels)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4800)!
        buffer.frameLength = 4800
        return buffer
    }

    private func sessionDirectory() throws -> URL {
        let session = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(
            at: session.appendingPathComponent("audio"), withIntermediateDirectories: true)
        return session
    }

    func testEachPartReportsTheTimeOfItsFirstBufferOnce() throws {
        let session = try sessionDirectory()
        defer { try? FileManager.default.removeItem(at: session) }
        let archive = MicArchive(outputURL: session.appendingPathComponent("audio/mic-001.m4a"))
        var started: [(String, UInt64?)] = []
        archive.onPartStarted = { url, hostTimeNs in started.append((url.lastPathComponent, hostTimeNs)) }

        try archive.write(buffer(1), hostTimeNs: 1_000)
        try archive.write(buffer(1), hostTimeNs: 2_000)
        // A format change opens the next part before its first buffer arrives.
        try archive.prepare(buffer(2).format)
        XCTAssertEqual(started.count, 1)
        try archive.write(buffer(2), hostTimeNs: 9_000)
        try archive.write(buffer(2), hostTimeNs: 10_000)
        // The device gave no host time for the first buffer of the third part.
        try archive.write(buffer(1), hostTimeNs: nil)
        archive.finish()

        XCTAssertEqual(started.map(\.0), ["mic-001.m4a", "mic-001-part-02.m4a", "mic-001-part-03.m4a"])
        XCTAssertEqual(started.map(\.1), [1_000, 9_000, nil])
    }

    func testRecordIsWrittenBesideTheSessionAndRemovedWhenTheTimeIsUnknown() throws {
        let session = try sessionDirectory()
        defer { try? FileManager.default.removeItem(at: session) }
        let audio = session.appendingPathComponent("audio/call-002.m4a")
        let recordURL = PartTimeline.recordURL(forAudio: audio)
        XCTAssertEqual(
            recordURL.path, session.appendingPathComponent(".recall/timeline/call-002.m4a.json").path)

        PartTimeline.record(audioURL: audio, source: "call", hostTimeNs: 123_456_789)
        let json = try JSONSerialization.jsonObject(with: Data(contentsOf: recordURL)) as? [String: Any]
        XCTAssertEqual(json?["file"] as? String, "call-002.m4a")
        XCTAssertEqual(json?["source"] as? String, "call")
        XCTAssertEqual(json?["host_time_ns"] as? UInt64, 123_456_789)
        XCTAssertNotNil(json?["unix_ms"] as? Int64)

        // A part with no host time must not leave an older time behind.
        PartTimeline.record(audioURL: audio, source: "call", hostTimeNs: nil)
        XCTAssertFalse(FileManager.default.fileExists(atPath: recordURL.path))
    }

    func testAClockJumpInsideAPartIsReportedOnce() {
        var clock = BufferClock()
        let step: UInt64 = 10_000_000 // 480 frames at 48 kHz
        // Steady buffers, with a little jitter, are not a jump.
        XCTAssertNil(clock.advance(hostTimeNs: 1_000_000_000, frames: 480, sampleRate: 48000))
        XCTAssertNil(clock.advance(hostTimeNs: 1_000_000_000 + step, frames: 480, sampleRate: 48000))
        XCTAssertNil(clock.advance(hostTimeNs: 1_000_000_000 + 2 * step + 3_000_000, frames: 480, sampleRate: 48000))
        // The next buffer arrives 610 ms after the one before it ended.
        let late = 1_000_000_000 + 3 * step + 3_000_000 + 610_000_000
        XCTAssertEqual(clock.advance(hostTimeNs: late, frames: 480, sampleRate: 48000), 610_000_000)
        XCTAssertNil(clock.advance(hostTimeNs: late + step, frames: 480, sampleRate: 48000))
        // An earlier stamp, or none, is not lost time.
        XCTAssertNil(clock.advance(hostTimeNs: late, frames: 480, sampleRate: 48000))
        XCTAssertNil(clock.advance(hostTimeNs: nil, frames: 480, sampleRate: 48000))
        XCTAssertNil(clock.advance(hostTimeNs: late + 5_000_000_000, frames: 480, sampleRate: 48000))
    }

    func testRollingStartsANewPartOnlyAfterAudioWasWritten() throws {
        let session = try sessionDirectory()
        defer { try? FileManager.default.removeItem(at: session) }
        let archive = MicArchive(outputURL: session.appendingPathComponent("audio/mic-001.m4a"))
        var started: [(String, UInt64?)] = []
        archive.onPartStarted = { url, hostTimeNs in started.append((url.lastPathComponent, hostTimeNs)) }

        // Nothing written yet: there is no part to end.
        archive.rollIfStarted()
        try archive.write(buffer(1), hostTimeNs: 1_000)
        archive.rollIfStarted()
        // The same format, and still a new part with its own start time.
        try archive.write(buffer(1), hostTimeNs: 700_000_000)
        // A part opened by a format change has no audio yet, so a jump seen on
        // its first buffer must not leave an empty file behind.
        try archive.prepare(buffer(2).format)
        archive.rollIfStarted()
        try archive.write(buffer(2), hostTimeNs: 900_000_000)
        archive.finish()

        XCTAssertEqual(started.map(\.0), ["mic-001.m4a", "mic-001-part-02.m4a", "mic-001-part-03.m4a"])
        XCTAssertEqual(started.map(\.1), [1_000, 700_000_000, 900_000_000])
        let files = try FileManager.default.contentsOfDirectory(atPath: session.appendingPathComponent("audio").path)
        XCTAssertEqual(files.sorted(), ["mic-001-part-02.m4a", "mic-001-part-03.m4a", "mic-001.m4a"])
    }

    func testAnInvalidHostTimeIsNotRecorded() {
        var stamp = AudioTimeStamp()
        stamp.mHostTime = 42
        stamp.mFlags = []
        XCTAssertNil(withUnsafePointer(to: &stamp) { PartTimeline.hostTimeNs($0) })
        stamp.mFlags = .hostTimeValid
        XCTAssertNotNil(withUnsafePointer(to: &stamp) { PartTimeline.hostTimeNs($0) })
        XCTAssertNil(PartTimeline.hostTimeNs(nil))
    }
}
