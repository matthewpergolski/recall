import AVFoundation
import Foundation

enum MicPCM {
    static func format(_ description: AudioStreamBasicDescription) -> AVAudioFormat? {
        var description = description
        guard description.mChannelsPerFrame > 0, description.mChannelsPerFrame <= 32,
              description.mSampleRate.isFinite, description.mSampleRate > 0 else { return nil }
        let layout = description.mChannelsPerFrame > 2
            ? AVAudioChannelLayout(layoutTag: kAudioChannelLayoutTag_DiscreteInOrder | description.mChannelsPerFrame)
            : nil
        return AVAudioFormat(streamDescription: &description, channelLayout: layout)
    }

    // A device can change channel count without changing its ID. The callback's
    // buffer list describes its actual packing; never stride it as cached mono.
    static func copy(_ data: UnsafePointer<AudioBufferList>, nominal: AVAudioFormat) -> AVAudioPCMBuffer? {
        let buffers = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: data))
        guard !buffers.isEmpty else { return nil }
        var description = nominal.streamDescription.pointee
        guard description.mFormatID == kAudioFormatLinearPCM,
              description.mBitsPerChannel > 0, description.mBitsPerChannel % 8 == 0 else { return nil }
        let planar = buffers.count > 1
        guard !planar || buffers.allSatisfy({ $0.mNumberChannels == 1 }) else { return nil }
        let channels = buffers.reduce(UInt32(0)) { $0 + $1.mNumberChannels }
        guard channels > 0, channels <= 32 else { return nil }
        description.mChannelsPerFrame = channels
        if planar { description.mFormatFlags |= kAudioFormatFlagIsNonInterleaved }
        else { description.mFormatFlags &= ~kAudioFormatFlagIsNonInterleaved }
        description.mBytesPerFrame = (description.mBitsPerChannel / 8) * (planar ? 1 : channels)
        description.mBytesPerPacket = description.mBytesPerFrame
        description.mFramesPerPacket = 1
        let stride = description.mBytesPerFrame
        guard let first = buffers.first, first.mDataByteSize > 0,
              first.mDataByteSize % stride == 0,
              buffers.allSatisfy({ $0.mData != nil && $0.mDataByteSize == first.mDataByteSize }),
              let format = format(description),
              let result = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: first.mDataByteSize / stride)
        else { return nil }
        result.frameLength = first.mDataByteSize / stride
        let destination = UnsafeMutableAudioBufferListPointer(result.mutableAudioBufferList)
        guard destination.count == buffers.count else { return nil }
        for index in buffers.indices {
            guard destination[index].mDataByteSize >= buffers[index].mDataByteSize else { return nil }
            memcpy(destination[index].mData!, buffers[index].mData!, Int(buffers[index].mDataByteSize))
        }
        return result
    }

    static func level(_ buffer: AVAudioPCMBuffer) -> Float? {
        var energy = 0.0
        var count = 0
        for data in UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList) {
            guard let pointer = data.mData else { continue }
            switch buffer.format.commonFormat {
            case .pcmFormatFloat32:
                let samples = UnsafeBufferPointer(start: pointer.assumingMemoryBound(to: Float.self), count: Int(data.mDataByteSize) / 4)
                for value in samples where value.isFinite { energy += Double(value) * Double(value); count += 1 }
            case .pcmFormatInt16:
                let samples = UnsafeBufferPointer(start: pointer.assumingMemoryBound(to: Int16.self), count: Int(data.mDataByteSize) / 2)
                for value in samples { let sample = Double(value) / 32768; energy += sample * sample; count += 1 }
            default: return nil
            }
        }
        guard count > 0 else { return nil }
        return energy > 0 ? Float(10 * log10(energy / Double(count))) : -160
    }

    static func speechBuffer(_ buffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        guard buffer.format.channelCount > 2 else { return buffer }
        guard buffer.format.commonFormat == .pcmFormatFloat32,
              let format = AVAudioFormat(standardFormatWithSampleRate: buffer.format.sampleRate, channels: 1),
              let mono = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: buffer.frameLength)
        else { return nil }
        mono.frameLength = buffer.frameLength
        let channels = Int(buffer.format.channelCount)
        let input = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
        let output = mono.floatChannelData![0]
        for frame in 0..<Int(buffer.frameLength) {
            var sum: Double = 0
            for channel in 0..<channels {
                let data = input[buffer.format.isInterleaved ? 0 : channel].mData!.assumingMemoryBound(to: Float.self)
                let sample = data[buffer.format.isInterleaved ? frame * channels + channel : frame]
                if sample.isFinite { sum += Double(sample) }
            }
            output[frame] = Float(sum / Double(channels))
        }
        return mono
    }
}

// Access only on the recorder's serial writer queue. Keep original channels;
// discrete multichannel AAC is not supported by the encoder on the tested Mac.
final class MicArchive {
    private let outputURL: URL
    private let stem: String
    private let initialPart: Int
    private var file: AVAudioFile?
    private var format: AVAudioFormat?
    private var part = 1
    var length: AVAudioFramePosition? { file?.length }

    init(outputURL: URL) {
        self.outputURL = outputURL
        let originalStem = outputURL.deletingPathExtension().lastPathComponent
        if let range = originalStem.range(of: "-part-", options: .backwards),
           let number = Int(originalStem[range.upperBound...]), number > 0 {
            stem = String(originalStem[..<range.lowerBound])
            initialPart = number
            part = number
        } else {
            stem = originalStem
            initialPart = 1
        }
    }

    func prepare(_ format: AVAudioFormat) throws {
        if self.format == format, file != nil { return }
        if file != nil { part += 1 }
        file = nil
        var path = part == initialPart ? outputURL : partURL()
        while part != initialPart && FileManager.default.fileExists(atPath: path.path) {
            part += 1
            path = partURL()
        }
        let multichannel = format.channelCount > 2
        var settings: [String: Any] = [
            AVFormatIDKey: multichannel ? kAudioFormatAppleLossless : kAudioFormatMPEG4AAC,
            AVSampleRateKey: format.sampleRate, AVNumberOfChannelsKey: format.channelCount
        ]
        if multichannel, let layout = format.channelLayout {
            settings[AVChannelLayoutKey] = Data(bytes: layout.layout, count: MemoryLayout<AudioChannelLayout>.size)
            settings[AVEncoderBitDepthHintKey] = 24
        } else { settings[AVEncoderAudioQualityKey] = AVAudioQuality.high.rawValue }
        file = try AVAudioFile(forWriting: path, settings: settings,
                              commonFormat: format.commonFormat, interleaved: format.isInterleaved)
        self.format = format
    }

    func write(_ buffer: AVAudioPCMBuffer) throws {
        try prepare(buffer.format)
        try file!.write(from: buffer)
    }

    func finish() { file = nil }

    private func partURL() -> URL {
        outputURL.deletingLastPathComponent().appendingPathComponent(String(format: "%@-part-%02d.m4a", stem, part))
    }
}
