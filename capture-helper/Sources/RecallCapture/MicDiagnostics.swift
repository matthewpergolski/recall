import AVFoundation
import CoreAudio
import Foundation

// Callback-owned counters. Only numeric diagnostics leave the audio callback;
// logging runs on the existing writer queue, never on the real-time thread.
final class MicInputDiagnostics: @unchecked Sendable {
    struct Window: Sendable {
        var elapsed: Double = 0
        var callbacks = 0
        var samples = 0
        var zeros = 0
        var nonfinite = 0
        var energy: Double = 0
        var peak: Double = 0
        var copiedFrames = 0
        var copiedEnergy: Double = 0
        var copiedSamples = 0
        var missingBuffers = 0
        var unsupportedBuffers = 0
        var mutedCallbacks = 0
        var maxCallbackGap: Double = 0

        var rmsDb: Double? {
            let finite = samples - nonfinite
            return finite > 0 && energy > 0 ? 10 * log10(energy / Double(finite)) : nil
        }

        mutating func add(_ values: UnsafeBufferPointer<Float>) {
            for value in values {
                samples += 1
                guard value.isFinite else { nonfinite += 1; continue }
                if value == 0 { zeros += 1 }
                let sample = Double(value)
                energy += sample * sample
                peak = max(peak, abs(sample))
            }
        }

        func log(writerDelay: Double, fileFrames: Int64?) {
            MicInputDiagnostics.emit([
                "type": "mic_pcm_diagnostic", "elapsed_seconds": elapsed,
                "callbacks": callbacks, "raw_samples": samples,
                "raw_zero_samples": zeros, "raw_nonfinite_samples": nonfinite,
                "raw_rms_dbfs": rmsDb as Any? ?? NSNull(),
                "raw_peak_dbfs": peak > 0 ? 20 * log10(peak) as Any : NSNull(),
                "copied_frames": copiedFrames, "missing_buffers": missingBuffers,
                "copied_rms_dbfs": copiedSamples > 0 && copiedEnergy > 0
                    ? 10 * log10(copiedEnergy / Double(copiedSamples)) as Any : NSNull(),
                "unsupported_buffers": unsupportedBuffers, "muted_callbacks": mutedCallbacks,
                "max_callback_gap_seconds": maxCallbackGap,
                "writer_delay_seconds": writerDelay,
                "file_frames_at_window_end": fileFrames as Any? ?? NSNull()
            ])
        }
    }

    private var window = Window()
    private var lastReport: Double = 0
    private var lastCallback: Double?

    func observe(_ data: UnsafePointer<AudioBufferList>?, format: AVAudioFormat?,
                 copiedBuffer: AVAudioPCMBuffer?, muted: Bool, elapsed: Double) -> Window? {
        window.callbacks += 1
        window.elapsed = elapsed
        window.copiedFrames += Int(copiedBuffer?.frameLength ?? 0)
        if let copiedBuffer, copiedBuffer.format.commonFormat == .pcmFormatFloat32 {
            var copied = Window()
            for buffer in UnsafeMutableAudioBufferListPointer(copiedBuffer.mutableAudioBufferList) {
                if let pointer = buffer.mData {
                    copied.add(UnsafeBufferPointer(start: pointer.assumingMemoryBound(to: Float.self),
                                                   count: Int(buffer.mDataByteSize) / MemoryLayout<Float>.size))
                }
            }
            window.copiedEnergy += copied.energy
            window.copiedSamples += copied.samples - copied.nonfinite
        }
        if muted { window.mutedCallbacks += 1 }
        if let lastCallback { window.maxCallbackGap = max(window.maxCallbackGap, elapsed - lastCallback) }
        lastCallback = elapsed
        if let data, let format {
            let asbd = format.streamDescription.pointee
            if asbd.mFormatID == kAudioFormatLinearPCM,
               asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0,
               asbd.mFormatFlags & kAudioFormatFlagIsBigEndian == 0,
               asbd.mBitsPerChannel == 32 {
                for buffer in UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: data)) {
                    if let pointer = buffer.mData {
                        window.add(UnsafeBufferPointer(start: pointer.assumingMemoryBound(to: Float.self),
                                                       count: Int(buffer.mDataByteSize) / MemoryLayout<Float>.size))
                    } else { window.missingBuffers += 1 }
                }
            } else { window.unsupportedBuffers += 1 }
        } else { window.missingBuffers += 1 }
        guard elapsed - lastReport >= 1 else { return nil }
        lastReport = elapsed
        let result = window
        window = Window()
        return result
    }

    static func emit(_ fields: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys]),
              let line = String(data: data, encoding: .utf8) else { return }
        FileHandle.standardError.write(Data((line + "\n").utf8))
    }

    // Read-only property inspection. Unsupported controls are reported, not guessed.
    static func describe(_ format: AudioStreamBasicDescription) -> [String: Any] {
        ["sample_rate": format.mSampleRate, "format_id": format.mFormatID,
         "channels": format.mChannelsPerFrame, "bytes_per_frame": format.mBytesPerFrame,
         "bytes_per_packet": format.mBytesPerPacket, "frames_per_packet": format.mFramesPerPacket,
         "bits_per_channel": format.mBitsPerChannel, "flags": format.mFormatFlags]
    }

    static func deviceSnapshot(id: AudioDeviceID, name: String?, uid: String?, elapsed: Double) {
        func read<T>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector,
                     _ scope: AudioObjectPropertyScope, _ element: AudioObjectPropertyElement,
                     _ initial: T) -> Any {
            var address = AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: element)
            var value = initial
            var size = UInt32(MemoryLayout<T>.size)
            let status = withUnsafeMutablePointer(to: &value) {
                AudioObjectGetPropertyData(object, &address, 0, nil, &size, $0)
            }
            return status == noErr ? value : ["unavailable_osstatus": status] as Any
        }
        let global = kAudioObjectPropertyScopeGlobal
        let input = kAudioObjectPropertyScopeInput
        var fields: [String: Any] = [
            "type": "mic_device_diagnostic", "elapsed_seconds": elapsed,
            "captured_device_id": id, "captured_device_name": name ?? "unknown",
            "captured_device_uid": uid ?? "unknown",
            "default_input_id": read(AudioObjectID(kAudioObjectSystemObject), kAudioHardwarePropertyDefaultInputDevice, global, 0, UInt32(0)),
            "default_output_id": read(AudioObjectID(kAudioObjectSystemObject), kAudioHardwarePropertyDefaultOutputDevice, global, 0, UInt32(0)),
            "hog_pid": read(id, kAudioDevicePropertyHogMode, global, 0, pid_t(-1)),
            "nominal_sample_rate": read(id, kAudioDevicePropertyNominalSampleRate, global, 0, Float64(0)),
            "input_data_source": read(id, kAudioDevicePropertyDataSource, input, 0, UInt32(0))
        ]
        for channel in 0...2 {
            fields["input_mute_\(channel)"] = read(id, kAudioDevicePropertyMute, input, UInt32(channel), UInt32(0))
            fields["input_volume_\(channel)"] = read(id, kAudioDevicePropertyVolumeScalar, input, UInt32(channel), Float32(0))
        }
        var address = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreamFormat, mScope: input, mElement: 0)
        var format = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        let status = AudioObjectGetPropertyData(id, &address, 0, nil, &size, &format)
        fields["format_osstatus"] = status
        if status == noErr {
            fields["format"] = describe(format)
        }
        address.mSelector = kAudioDevicePropertyStreams
        var streamSize: UInt32 = 0
        let streamStatus = AudioObjectGetPropertyDataSize(id, &address, 0, nil, &streamSize)
        fields["input_streams_size_osstatus"] = streamStatus
        if streamStatus == noErr, streamSize > 0, streamSize <= 4096,
           Int(streamSize) % MemoryLayout<AudioStreamID>.size == 0 {
            var streams = [AudioStreamID](repeating: 0, count: Int(streamSize) / MemoryLayout<AudioStreamID>.size)
            let fetchStatus = streams.withUnsafeMutableBytes {
                AudioObjectGetPropertyData(id, &address, 0, nil, &streamSize, $0.baseAddress!)
            }
            fields["input_streams_osstatus"] = fetchStatus
            if fetchStatus == noErr {
                fields["input_streams"] = streams.map { stream -> [String: Any] in
                    var result: [String: Any] = ["id": stream]
                    for (name, selector) in [("virtual", kAudioStreamPropertyVirtualFormat),
                                             ("physical", kAudioStreamPropertyPhysicalFormat)] {
                        var address = AudioObjectPropertyAddress(mSelector: selector, mScope: global, mElement: 0)
                        var value = AudioStreamBasicDescription()
                        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
                        let status = AudioObjectGetPropertyData(stream, &address, 0, nil, &size, &value)
                        result[name] = status == noErr ? describe(value) : ["unavailable_osstatus": status]
                    }
                    return result
                }
            }
        }
        emit(fields)
    }
}
