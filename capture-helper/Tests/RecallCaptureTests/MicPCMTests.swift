import AVFoundation
import XCTest
@testable import RecallCapture

final class MicPCMTests: XCTestCase {
    private func description(_ channels: UInt32, planar: Bool = false) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription(mSampleRate: 48000, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: 9 | (planar ? kAudioFormatFlagIsNonInterleaved : 0),
            mBytesPerPacket: 4 * (planar ? 1 : channels), mFramesPerPacket: 1,
            mBytesPerFrame: 4 * (planar ? 1 : channels), mChannelsPerFrame: channels, mBitsPerChannel: 32, mReserved: 0)
    }

    private func buffer(_ channels: UInt32, planar: Bool = false) -> AVAudioPCMBuffer {
        let format = MicPCM.format(description(channels, planar: planar))!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4800)!
        buffer.frameLength = 4800
        let buffers = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
        for index in buffers.indices {
            let values = buffers[index].mData!.assumingMemoryBound(to: Float.self)
            for frame in 0..<4800 {
                for channel in 0..<(planar ? 1 : Int(channels)) {
                    let actual = planar ? index : channel
                    values[frame * (planar ? 1 : Int(channels)) + channel] = Float(sin(Double(frame) * Double(actual + 1) * 0.05)) * 0.1
                }
            }
        }
        return buffer
    }

    func testReportedThreeChannelFormatNeedsLayout() {
        var reported = description(3)
        XCTAssertNil(AVAudioFormat(streamDescription: &reported))
        let supported = MicPCM.format(reported)
        XCTAssertEqual(supported?.channelCount, 3)
        XCTAssertEqual(supported?.streamDescription.pointee.mBytesPerFrame, 12)
    }

    func testMonoToThreeChannelsDoesNotUseStaleStrideOrTruncate() throws {
        let mono = MicPCM.format(description(1))!
        let source = buffer(3)
        let copied = try XCTUnwrap(MicPCM.copy(source.audioBufferList, nominal: mono))
        XCTAssertEqual(copied.frameLength, 4800)
        XCTAssertEqual(copied.format.channelCount, 3)
        let expected = source.audioBufferList.pointee.mBuffers
        let actual = copied.audioBufferList.pointee.mBuffers
        XCTAssertEqual(Data(bytes: expected.mData!, count: Int(expected.mDataByteSize)),
                       Data(bytes: actual.mData!, count: Int(actual.mDataByteSize)))
        XCTAssertNotNil(MicPCM.level(copied))
    }

    func testThreeChannelsBackToMonoAndPlanarInput() throws {
        let three = MicPCM.format(description(3))!
        let mono = buffer(1)
        XCTAssertEqual(MicPCM.copy(mono.audioBufferList, nominal: three)?.frameLength, 4800)
        let planar = buffer(3, planar: true)
        let copied = try XCTUnwrap(MicPCM.copy(planar.audioBufferList, nominal: three))
        XCTAssertFalse(copied.format.isInterleaved)
        XCTAssertEqual(copied.frameLength, 4800)
        XCTAssertEqual(MicPCM.level(planar)!, MicPCM.level(copied)!, accuracy: 0.0001)
    }

    func testArchiveKeepsAllChannelsAcrossFormatSwitches() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let archive = MicArchive(outputURL: directory.appendingPathComponent("mic-001.m4a"))
        try archive.write(buffer(1))
        let source = buffer(3)
        try archive.write(source)
        try archive.write(buffer(1))
        archive.finish()
        let file = try AVAudioFile(forReading: directory.appendingPathComponent("mic-001-part-02.m4a"))
        XCTAssertEqual(file.processingFormat.channelCount, 3)
        XCTAssertEqual(file.length, 4800)
        XCTAssertEqual(file.fileFormat.streamDescription.pointee.mFormatID, kAudioFormatAppleLossless)
        let decoded = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: 4800)!
        try file.read(into: decoded)
        let packed = source.audioBufferList.pointee.mBuffers.mData!.assumingMemoryBound(to: Float.self)
        for channel in 0..<3 {
            for frame in 0..<4800 {
                XCTAssertEqual(decoded.floatChannelData![channel][frame], packed[frame * 3 + channel], accuracy: 0.000001)
            }
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.appendingPathComponent("mic-001-part-03.m4a").path))
    }

    func testSpeechPreviewIncludesThirdChannelWithoutChangingArchiveBuffer() throws {
        let source = buffer(3)
        let data = source.audioBufferList.pointee.mBuffers.mData!.assumingMemoryBound(to: Float.self)
        for frame in 0..<4800 { data[frame * 3] = 0; data[frame * 3 + 1] = 0; data[frame * 3 + 2] = 0.3 }
        let preview = try XCTUnwrap(MicPCM.speechBuffer(source))
        XCTAssertEqual(preview.format.channelCount, 1)
        XCTAssertEqual(preview.floatChannelData![0][100], 0.1, accuracy: 0.00001)
        XCTAssertEqual(data[302], 0.3)
    }

    func testFormatChangeAfterHelperRestartUsesDiscoverablePartName() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let archive = MicArchive(outputURL: directory.appendingPathComponent("mic-001-part-02.m4a"))
        try archive.write(buffer(1))
        try archive.write(buffer(3))
        archive.finish()
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.appendingPathComponent("mic-001-part-03.m4a").path))
    }

    func testFFmpegReadsThirdChannelFromActualArchive() throws {
        guard let ffmpeg = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].first(where: { FileManager.default.isExecutableFile(atPath: $0) })
        else { throw XCTSkip("ffmpeg unavailable") }
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("mic-001.m4a")
        let source = buffer(3)
        let data = source.audioBufferList.pointee.mBuffers.mData!.assumingMemoryBound(to: Float.self)
        for frame in 0..<4800 { data[frame * 3] = 0; data[frame * 3 + 1] = 0 }
        let archive = MicArchive(outputURL: path)
        try archive.write(source)
        archive.finish()
        let output = directory.appendingPathComponent("mono.wav")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: ffmpeg)
        process.arguments = ["-v", "error", "-i", path.path, "-af", "pan=mono|c0<c0+c1+c2", "-c:a", "pcm_f32le", output.path]
        try process.run()
        process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0)
        let read = try AVAudioFile(forReading: output)
        let decoded = AVAudioPCMBuffer(pcmFormat: read.processingFormat, frameCapacity: 4800)!
        try read.read(into: decoded)
        XCTAssertEqual(decoded.frameLength, 4800)
        for frame in 0..<4800 {
            XCTAssertEqual(decoded.floatChannelData![0][frame], data[frame * 3 + 2] / 3, accuracy: 0.000001)
        }
    }
}
