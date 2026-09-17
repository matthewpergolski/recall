import AVFoundation
import CoreMedia
import Foundation
#if compiler(>=6.2)
import Speech
#endif

struct TranscribeFileOptions {
    let audioURL: URL
    let outBase: URL
}

struct AppleTranscribeStatus: Encodable {
    let available: Bool
    let locale: String?
    let installedLocales: [String]
    let message: String?
}

struct AppleTranscriptFile: Encodable {
    let type: String
    let engine: String
    let locale: String
    let segments: [AppleTranscriptSegment]
}

struct AppleTranscriptSegment: Encodable {
    let start: Double
    let end: Double
    let text: String
}

struct TranscribeErrorResponse: Encodable {
    let type: String
    let message: String
    let available: Bool
}

extension RecallCapture {
    static func parseTranscribeFileOptions(_ args: [String]) throws -> TranscribeFileOptions {
        var audioURL: URL?
        var outBase: URL?
        var index = 0

        while index < args.count {
            let arg = args[index]
            switch arg {
            case "--audio":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                audioURL = URL(fileURLWithPath: args[index + 1])
                index += 2
            case "--out":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                outBase = URL(fileURLWithPath: args[index + 1])
                index += 2
            default:
                fputs("Ignoring unknown transcribe-file option: \(arg)\n", stderr)
                index += 1
            }
        }

        guard let audioURL else {
            throw CaptureError.missingAudioPath
        }
        guard let outBase else {
            throw CaptureError.missingTranscribeOutPath
        }

        return TranscribeFileOptions(audioURL: audioURL, outBase: outBase)
    }

    static func transcribeStatus() async throws {
        try printJSON(await appleTranscribeStatus())
    }

    static func transcribeFile(_ options: TranscribeFileOptions) async throws {
        guard FileManager.default.fileExists(atPath: options.audioURL.path) else {
            throw CaptureError.transcribeFailed("Audio file not found: \(options.audioURL.path)")
        }

        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else {
            throw CaptureError.appleSpeechUnavailable(
                "Apple SpeechAnalyzer requires macOS 26 or later"
            )
        }

        let prepared = try await prepareAppleTranscriber()
        let segments = try await transcribeAudioFile(
            options.audioURL,
            transcriber: prepared.transcriber
        )
        try writeTranscriptArtifacts(
            outBase: options.outBase,
            locale: appleLocaleIdentifier(prepared.locale),
            segments: segments
        )
        #else
        throw CaptureError.appleSpeechUnavailable(
            "Rebuild capture-helper with Xcode 26 or later for Apple SpeechAnalyzer"
        )
        #endif
    }

    static func appleTranscribeStatus() async -> AppleTranscribeStatus {
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else {
            return AppleTranscribeStatus(
                available: false,
                locale: nil,
                installedLocales: [],
                message: "Apple SpeechAnalyzer requires macOS 26 or later"
            )
        }

        do {
            let prepared = try await prepareAppleTranscriber()
            let installed = await SpeechTranscriber.installedLocales.map(appleLocaleIdentifier)
            return AppleTranscribeStatus(
                available: true,
                locale: appleLocaleIdentifier(prepared.locale),
                installedLocales: installed,
                message: nil
            )
        } catch {
            let installed = await SpeechTranscriber.installedLocales.map(appleLocaleIdentifier)
            return AppleTranscribeStatus(
                available: false,
                locale: nil,
                installedLocales: installed,
                message: "\(error)"
            )
        }
        #else
        return AppleTranscribeStatus(
            available: false,
            locale: nil,
            installedLocales: [],
            message: "Rebuild capture-helper with Xcode 26 or later for Apple SpeechAnalyzer"
        )
        #endif
    }

    #if compiler(>=6.2)
    @available(macOS 26.0, *)
    static func resolvedAppleSpeechLocale() async throws -> Locale {
        guard SpeechTranscriber.isAvailable else {
            throw CaptureError.appleSpeechUnavailable(
                "Apple SpeechAnalyzer is not available on this Mac"
            )
        }

        if let preferred = await SpeechTranscriber.supportedLocale(
            equivalentTo: Locale(identifier: "en_US")
        ) {
            return preferred
        }
        if let current = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) {
            return current
        }
        throw CaptureError.appleSpeechUnavailable(
            "Apple SpeechAnalyzer has no supported locale matching en_US or the current locale"
        )
    }

    @available(macOS 26.0, *)
    static func requireAppleSpeechAssets(
        for transcriber: SpeechTranscriber,
        locale: Locale
    ) async throws {
        if try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) != nil {
            throw CaptureError.appleSpeechModelNotInstalled(appleLocaleIdentifier(locale))
        }
    }

    @available(macOS 26.0, *)
    private static func prepareAppleTranscriber() async throws -> (
        transcriber: SpeechTranscriber,
        locale: Locale
    ) {
        let locale = try await resolvedAppleSpeechLocale()
        let transcriber = SpeechTranscriber(locale: locale, preset: .transcription)
        try await requireAppleSpeechAssets(for: transcriber, locale: locale)
        return (transcriber, locale)
    }

    @available(macOS 26.0, *)
    private static func transcribeAudioFile(
        _ audioURL: URL,
        transcriber: SpeechTranscriber
    ) async throws -> [AppleTranscriptSegment] {
        let file = try openAudioFile(audioURL)
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let collector = TranscriptCollector()
        let listen = Task {
            do {
                for try await result in transcriber.results {
                    await collector.add(result)
                }
            } catch {
                await collector.fail(error)
            }
        }

        do {
            if let last = try await analyzer.analyzeSequence(from: file) {
                try await analyzer.finalizeAndFinish(through: last)
            } else {
                try await analyzer.finalizeAndFinishThroughEndOfInput()
            }
        } catch {
            listen.cancel()
            throw CaptureError.transcribeFailed("\(error)")
        }

        _ = await listen.result
        if let error = await collector.error() {
            throw CaptureError.transcribeFailed("\(error)")
        }
        return await collector.segments()
    }

    private static func openAudioFile(_ url: URL) throws -> AVAudioFile {
        do {
            return try AVAudioFile(forReading: url)
        } catch {
            return try AVAudioFile(
                forReading: url,
                commonFormat: .pcmFormatInt16,
                interleaved: false
            )
        }
    }

    private static func writeTranscriptArtifacts(
        outBase: URL,
        locale: String,
        segments: [AppleTranscriptSegment]
    ) throws {
        let parent = outBase.deletingLastPathComponent()
        if !parent.path.isEmpty, parent.path != ".", parent.path != outBase.path {
            try FileManager.default.createDirectory(
                at: parent,
                withIntermediateDirectories: true
            )
        }

        let payload = AppleTranscriptFile(
            type: "transcript",
            engine: "apple",
            locale: locale,
            segments: segments
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(payload).write(
            to: urlByAddingExtension(outBase, "json"),
            options: .atomic
        )
        try webvtt(from: segments).write(
            to: urlByAddingExtension(outBase, "vtt"),
            atomically: true,
            encoding: .utf8
        )
        try joinedText(from: segments).write(
            to: urlByAddingExtension(outBase, "txt"),
            atomically: true,
            encoding: .utf8
        )
    }

    private static func urlByAddingExtension(_ base: URL, _ ext: String) -> URL {
        URL(fileURLWithPath: base.path + "." + ext)
    }

    private static func webvtt(from segments: [AppleTranscriptSegment]) -> String {
        var lines = ["WEBVTT", ""]
        for segment in segments {
            lines.append("\(formatVttTime(segment.start)) --> \(formatVttTime(segment.end))")
            lines.append(segment.text)
            lines.append("")
        }
        return lines.joined(separator: "\n")
    }

    private static func joinedText(from segments: [AppleTranscriptSegment]) -> String {
        segments
            .map(\.text)
            .filter { !$0.isEmpty }
            .joined(separator: "\n")
    }

    private static func formatVttTime(_ seconds: Double) -> String {
        let totalMs = max(0, Int((seconds * 1000).rounded()))
        let hours = totalMs / 3_600_000
        let minutes = (totalMs % 3_600_000) / 60_000
        let secs = (totalMs % 60_000) / 1000
        let millis = totalMs % 1000
        return String(format: "%02d:%02d:%02d.%03d", hours, minutes, secs, millis)
    }

    static func appleLocaleIdentifier(_ locale: Locale) -> String {
        locale.identifier(.bcp47)
    }
    #endif
}

#if compiler(>=6.2)
@available(macOS 26.0, *)
actor TranscriptCollector {
    private var collected: [AppleTranscriptSegment] = []
    private var collectError: Error?

    func add(_ result: SpeechTranscriber.Result) {
        guard result.isFinal else {
            return
        }
        let text = String(result.text.characters)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            return
        }
        let range = result.range
        let start = range.start.isValid ? range.start.seconds : 0
        let end = range.end.isValid ? range.end.seconds : start
        collected.append(
            AppleTranscriptSegment(
                start: start,
                end: max(end, start),
                text: text
            )
        )
    }

    func fail(_ error: Error) {
        collectError = error
    }

    func error() -> Error? {
        collectError
    }

    func segments() -> [AppleTranscriptSegment] {
        collected
    }
}
#endif
