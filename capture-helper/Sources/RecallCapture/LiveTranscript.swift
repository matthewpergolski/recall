@preconcurrency import AVFoundation
import CoreMedia
import Foundation
#if compiler(>=6.2)
import Speech
#endif

struct LiveTranscriptEvent: Encodable {
    let type: String
    let source: String
    let text: String
    let volatile: Bool
    let elapsedSeconds: Double
    let revision: Int
}

struct LiveTranscriptUnavailableEvent: Encodable {
    let type: String
    let source: String
    let message: String
}

final class LiveTranscriptFeed: @unchecked Sendable {
    private let source: String
    private let startedAt: Date
    private let session: AnyObject?

    init(source: String, startedAt: Date) {
        self.source = source
        self.startedAt = startedAt
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            session = LiveSpeechSession(source: source, startedAt: startedAt)
        } else {
            session = nil
            try? RecallCapture.printJSONLine(LiveTranscriptUnavailableEvent(
                type: "live_transcript_unavailable",
                source: source,
                message: "Apple SpeechAnalyzer requires macOS 26 or later"
            ))
        }
        #else
        session = nil
        try? RecallCapture.printJSONLine(LiveTranscriptUnavailableEvent(
            type: "live_transcript_unavailable",
            source: source,
            message: "Rebuild capture-helper with Xcode 26 or later for live transcript"
        ))
        #endif
    }

    func start() {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            (session as? LiveSpeechSession)?.start()
        }
        #endif
    }

    func enqueue(_ buffer: AVAudioPCMBuffer) {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            (session as? LiveSpeechSession)?.enqueue(buffer)
        }
        #endif
    }

    func finish() {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            (session as? LiveSpeechSession)?.finish()
        }
        #endif
    }
}

#if compiler(>=6.2)
@available(macOS 26.0, *)
final class LiveSpeechSession: @unchecked Sendable {
    private static let maxPendingBuffers = 24
    private static let yieldFrameCount: AVAudioFrameCount = 8192

    private let source: String
    private let startedAt: Date
    private let speechQueue = DispatchQueue(label: "recall.live-speech")

    private let lock = NSLock()
    private var pending: [AVAudioPCMBuffer] = []
    private var pumping = false
    private var finished = false
    private var ready = false
    private var dead = false
    private var unavailableEmitted = false
    private var revision = 0
    private var nextStart = CMTime.zero
    private var accumulator: AVAudioPCMBuffer?
    private var inputBuilder: AsyncStream<AnalyzerInput>.Continuation?
    private var analyzer: SpeechAnalyzer?
    private var analyzerFormat: AVAudioFormat?
    private var converter: AVAudioConverter?
    private var converterSourceFormat: AVAudioFormat?
    private var analyzeTask: Task<CMTime?, Error>?
    private var resultsTask: Task<Void, Never>?

    init(source: String, startedAt: Date) {
        self.source = source
        self.startedAt = startedAt
    }

    func start() {
        Task { [weak self] in
            await self?.prepare()
        }
    }

    func enqueue(_ buffer: AVAudioPCMBuffer) {
        guard !isDead() else {
            return
        }
        guard let copy = copyPCMBuffer(buffer), copy.frameLength > 0 else {
            return
        }
        speechQueue.async { [weak self] in
            guard let self, !self.finished, !self.isDead() else {
                return
            }
            do {
                while self.pending.count >= Self.maxPendingBuffers {
                    self.pending.removeFirst()
                }
                self.pending.append(copy)
                if self.ready && !self.pumping {
                    self.pumping = true
                    try self.pump()
                }
            } catch {
                self.markDead("\(error)")
            }
        }
    }

    func finish() {
        if isDead() {
            speechQueue.async { [weak self] in
                self?.finished = true
                self?.pending.removeAll()
                self?.inputBuilder?.finish()
                self?.inputBuilder = nil
            }
            return
        }
        let semaphore = DispatchSemaphore(value: 0)
        speechQueue.async { [weak self] in
            guard let self else {
                semaphore.signal()
                return
            }
            self.finished = true
            do {
                try self.flushAccumulator()
            } catch {
                self.markDead("\(error)")
            }
            self.inputBuilder?.finish()
            self.inputBuilder = nil
            let analyzeTask = self.analyzeTask
            let analyzer = self.analyzer
            let resultsTask = self.resultsTask
            Task {
                defer { semaphore.signal() }
                if self.isDead() {
                    resultsTask?.cancel()
                    return
                }
                if let analyzeTask {
                    do {
                        if let last = try await analyzeTask.value {
                            try await analyzer?.finalizeAndFinish(through: last)
                        } else {
                            try await analyzer?.finalizeAndFinishThroughEndOfInput()
                        }
                    } catch {
                        self.markDead("\(error)")
                    }
                }
                try? await Task.sleep(for: .milliseconds(250))
                resultsTask?.cancel()
            }
        }
        _ = semaphore.wait(timeout: .now() + 1.2)
    }

    private func prepare() async {
        do {
            let locale = try await RecallCapture.resolvedAppleSpeechLocale()
            let transcriber = SpeechTranscriber(
                locale: locale,
                transcriptionOptions: [],
                reportingOptions: [.volatileResults, .fastResults],
                attributeOptions: [.audioTimeRange]
            )
            try await RecallCapture.requireAppleSpeechAssets(for: transcriber, locale: locale)
            let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
                ?? AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: false)
            guard let format else {
                throw CaptureError.audioFormatUnavailable
            }
            let analyzer = SpeechAnalyzer(modules: [transcriber])
            try await analyzer.prepareToAnalyze(in: format)
            let (inputSequence, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
            let resultsTask = Task { [weak self] in
                do {
                    for try await result in transcriber.results {
                        self?.emitResult(result)
                    }
                } catch {
                    self?.markDead("\(error)")
                }
            }
            let analyzeTask = Task { [weak self] in
                do {
                    return try await analyzer.analyzeSequence(inputSequence)
                } catch {
                    self?.markDead("\(error)")
                    throw error
                }
            }
            speechQueue.async { [weak self] in
                guard let self else {
                    return
                }
                if self.isDead() || self.finished {
                    continuation.finish()
                    resultsTask.cancel()
                    return
                }
                self.analyzer = analyzer
                self.analyzerFormat = format
                self.nextStart = CMTime(value: 0, timescale: CMTimeScale(format.sampleRate))
                self.resultsTask = resultsTask
                self.analyzeTask = analyzeTask
                self.inputBuilder = continuation
                self.ready = true
                if !self.pending.isEmpty && !self.pumping {
                    self.pumping = true
                    do {
                        try self.pump()
                    } catch {
                        self.markDead("\(error)")
                    }
                }
            }
        } catch {
            markDead("\(error)")
        }
    }

    private func pump() throws {
        while true {
            if isDead() || (finished && pending.isEmpty) {
                pumping = false
                return
            }
            guard ready, let builder = inputBuilder, let format = analyzerFormat else {
                pumping = false
                return
            }
            guard let buffer = pending.isEmpty ? nil : pending.removeFirst() else {
                pumping = false
                return
            }
            guard let converted = convert(buffer, to: format) else {
                continue
            }
            try appendAndYield(converted, builder: builder, format: format)
        }
    }

    private func appendAndYield(
        _ buffer: AVAudioPCMBuffer,
        builder: AsyncStream<AnalyzerInput>.Continuation,
        format: AVAudioFormat
    ) throws {
        let needed = Self.yieldFrameCount
        if buffer.frameLength >= needed {
            try flushAccumulator()
            try yieldBuffer(buffer, builder: builder)
            return
        }
        if accumulator == nil {
            accumulator = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: needed)
            accumulator?.frameLength = 0
        }
        if let accumulator, append(buffer, onto: accumulator) {
            if accumulator.frameLength >= needed {
                try flushAccumulator()
            }
            return
        }
        try flushAccumulator()
        if buffer.frameLength >= needed {
            try yieldBuffer(buffer, builder: builder)
            return
        }
        accumulator = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: needed)
        accumulator?.frameLength = 0
        if let accumulator, append(buffer, onto: accumulator) {
            if accumulator.frameLength >= needed {
                try flushAccumulator()
            }
        } else {
            try yieldBuffer(buffer, builder: builder)
        }
    }

    private func flushAccumulator() throws {
        guard let accumulator, accumulator.frameLength > 0, let builder = inputBuilder else {
            self.accumulator = nil
            return
        }
        if let copy = copyPCMBuffer(accumulator) {
            try yieldBuffer(copy, builder: builder)
        }
        accumulator.frameLength = 0
        self.accumulator = accumulator
    }

    private func yieldBuffer(
        _ buffer: AVAudioPCMBuffer,
        builder: AsyncStream<AnalyzerInput>.Continuation
    ) throws {
        if isDead() {
            return
        }
        let start = nextStart
        let duration = CMTime(
            value: CMTimeValue(buffer.frameLength),
            timescale: CMTimeScale(max(buffer.format.sampleRate, 1))
        )
        nextStart = CMTimeAdd(nextStart, duration)
        builder.yield(AnalyzerInput(buffer: buffer, bufferStartTime: start))
    }

    private func append(_ source: AVAudioPCMBuffer, onto dest: AVAudioPCMBuffer) -> Bool {
        let available = dest.frameCapacity - dest.frameLength
        guard available > 0, source.frameLength <= available else {
            return false
        }
        let destOffset = Int(dest.frameLength)
        let bytesPerSample: Int
        switch dest.format.commonFormat {
        case .pcmFormatInt16:
            bytesPerSample = MemoryLayout<Int16>.size
        case .pcmFormatFloat32:
            bytesPerSample = MemoryLayout<Float>.size
        default:
            bytesPerSample = max(Int(dest.format.streamDescription.pointee.mBytesPerFrame), 1)
        }
        let byteOffset = destOffset * bytesPerSample
        let src = UnsafeMutableAudioBufferListPointer(source.mutableAudioBufferList)
        let dst = UnsafeMutableAudioBufferListPointer(dest.mutableAudioBufferList)
        for index in 0..<min(src.count, dst.count) {
            guard let srcData = src[index].mData, let dstData = dst[index].mData else {
                return false
            }
            memcpy(
                dstData.advanced(by: byteOffset),
                srcData,
                Int(src[index].mDataByteSize)
            )
        }
        dest.frameLength += source.frameLength
        return true
    }

    private func convert(_ buffer: AVAudioPCMBuffer, to dest: AVAudioFormat) -> AVAudioPCMBuffer? {
        if buffer.format.sampleRate == dest.sampleRate,
           buffer.format.channelCount == dest.channelCount,
           buffer.format.commonFormat == dest.commonFormat,
           buffer.format.isInterleaved == dest.isInterleaved
        {
            return buffer
        }
        if converter == nil || converterSourceFormat != buffer.format {
            converter = AVAudioConverter(from: buffer.format, to: dest)
            converter?.primeMethod = .none
            converterSourceFormat = buffer.format
        }
        guard let converter else {
            return nil
        }
        let ratio = dest.sampleRate / buffer.format.sampleRate
        let outFrames = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 32
        guard let converted = AVAudioPCMBuffer(pcmFormat: dest, frameCapacity: max(outFrames, 1))
        else {
            return nil
        }
        final class Once: @unchecked Sendable { var done = false }
        let once = Once()
        var error: NSError?
        converter.convert(to: converted, error: &error) { _, status in
            if once.done {
                status.pointee = .noDataNow
                return nil
            }
            once.done = true
            status.pointee = .haveData
            return buffer
        }
        if error != nil {
            return nil
        }
        return converted
    }

    private func emitResult(_ result: SpeechTranscriber.Result) {
        if isDead() {
            return
        }
        let text = String(result.text.characters)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            return
        }
        speechQueue.async { [weak self] in
            guard let self, !self.isDead() else {
                return
            }
            self.revision += 1
            let current = self.revision
            try? RecallCapture.printJSONLine(LiveTranscriptEvent(
                type: "live_transcript",
                source: self.source,
                text: text,
                volatile: !result.isFinal,
                elapsedSeconds: Date().timeIntervalSince(self.startedAt),
                revision: current
            ))
        }
    }

    private func isDead() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return dead
    }

    private func markDead(_ message: String) {
        lock.lock()
        let shouldEmit = !unavailableEmitted
        dead = true
        unavailableEmitted = true
        lock.unlock()
        if shouldEmit {
            try? RecallCapture.printJSONLine(LiveTranscriptUnavailableEvent(
                type: "live_transcript_unavailable",
                source: source,
                message: message
            ))
        }
        speechQueue.async { [weak self] in
            guard let self else {
                return
            }
            self.ready = false
            self.pending.removeAll()
            self.pumping = false
            self.inputBuilder?.finish()
            self.inputBuilder = nil
            self.resultsTask?.cancel()
        }
    }
}
#endif

func copyPCMBuffer(_ buffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
    guard let copy = AVAudioPCMBuffer(
        pcmFormat: buffer.format,
        frameCapacity: max(buffer.frameLength, 1)
    ) else {
        return nil
    }
    copy.frameLength = buffer.frameLength
    let src = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
    let dst = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
    for index in 0..<min(src.count, dst.count) {
        guard let srcData = src[index].mData, let dstData = dst[index].mData else {
            continue
        }
        memcpy(dstData, srcData, min(Int(src[index].mDataByteSize), Int(dst[index].mDataByteSize)))
    }
    return copy
}
