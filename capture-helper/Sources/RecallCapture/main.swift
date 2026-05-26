import AppKit
import AudioToolbox
@preconcurrency import AVFoundation
import CoreAudio
import CoreMedia
import Foundation
@preconcurrency import ScreenCaptureKit

struct SourceList: Codable {
    let type: String
    let version: String
    let generatedAtUnix: Int
    let candidates: [AppCandidate]
    let microphones: [AudioDevice]
    let permissions: Permissions
}

struct AppCandidate: Codable {
    let kind: String
    let name: String
    let bundleIdentifier: String?
    let processIdentifier: Int
    let confidence: String
    let reason: String
}

struct AudioDevice: Codable {
    let kind: String
    let name: String
    let uniqueID: String
}

struct Permissions: Codable {
    let microphone: String
}

struct CaptureEvent: Codable {
    let type: String
    let source: String?
    let path: String?
    let elapsedSeconds: Double?
    let levelDb: Float?
    let message: String?
    let deviceName: String?
    let deviceID: String?

    init(
        type: String,
        source: String?,
        path: String?,
        elapsedSeconds: Double?,
        levelDb: Float?,
        message: String?,
        deviceName: String? = nil,
        deviceID: String? = nil
    ) {
        self.type = type
        self.source = source
        self.path = path
        self.elapsedSeconds = elapsedSeconds
        self.levelDb = levelDb
        self.message = message
        self.deviceName = deviceName
        self.deviceID = deviceID
    }
}

struct DefaultInputDevice: Equatable {
    let id: AudioDeviceID
    let name: String
    let uid: String
}

struct RecordMicOptions {
    let sessionDir: URL
    let durationSeconds: TimeInterval
    let stopFile: URL?
}

struct RecordSystemOptions {
    let sessionDir: URL
    let durationSeconds: TimeInterval
    let stopFile: URL?
}

final class PermissionResult: @unchecked Sendable {
    var granted = false
}

enum CaptureError: Error, CustomStringConvertible {
    case missingValue(String)
    case invalidDuration(String)
    case missingSessionDir
    case microphonePermissionDenied(String)
    case recorderCreationFailed
    case noShareableDisplays
    case assetWriterUnavailable
    case processTapCreationFailed(OSStatus)
    case processTapReadFailed(String)
    case aggregateDeviceCreationFailed(OSStatus)
    case audioDeviceIOProcCreationFailed(OSStatus)
    case audioDeviceStartFailed(OSStatus)
    case audioObjectPropertyFailed(String, OSStatus)
    case audioFormatUnavailable
    case unsupportedOS(String)

    var description: String {
        switch self {
        case .missingValue(let flag):
            return "\(flag) requires a value"
        case .invalidDuration(let value):
            return "Invalid --duration value: \(value)"
        case .missingSessionDir:
            return "record-mic requires --session-dir <path>"
        case .microphonePermissionDenied(let status):
            return "Microphone permission is \(status)"
        case .recorderCreationFailed:
            return "Failed to start microphone recorder"
        case .noShareableDisplays:
            return "ScreenCaptureKit did not return a display to capture"
        case .assetWriterUnavailable:
            return "Failed to create the system audio writer"
        case .processTapCreationFailed(let status):
            return "Failed to create CoreAudio process tap: OSStatus \(status)"
        case .processTapReadFailed(let message):
            return "Failed to read CoreAudio process tap audio: \(message)"
        case .aggregateDeviceCreationFailed(let status):
            return "Failed to create CoreAudio aggregate device: OSStatus \(status)"
        case .audioDeviceIOProcCreationFailed(let status):
            return "Failed to create CoreAudio device IO callback: OSStatus \(status)"
        case .audioDeviceStartFailed(let status):
            return "Failed to start CoreAudio aggregate device: OSStatus \(status)"
        case .audioObjectPropertyFailed(let property, let status):
            return "Failed to read CoreAudio property \(property): OSStatus \(status)"
        case .audioFormatUnavailable:
            return "Failed to resolve CoreAudio tap audio format"
        case .unsupportedOS(let message):
            return message
        }
    }
}

@main
struct RecallCapture {
    static func main() async throws {
        let args = Array(CommandLine.arguments.dropFirst())

        switch args.first {
        case nil, "help", "--help", "-h":
            printHelp()
        case "list-sources":
            try printJSON(listSources())
        case "record-mic":
            do {
                try recordMic(parseRecordMicOptions(Array(args.dropFirst())))
            } catch {
                try printJSONLine(CaptureEvent(
                    type: "error",
                    source: "mic",
                    path: nil,
                    elapsedSeconds: nil,
                    levelDb: nil,
                    message: "\(error)"
                ))
                Foundation.exit(1)
            }
        case "record-system":
            do {
                try await recordSystem(parseRecordSystemOptions(Array(args.dropFirst())))
            } catch {
                try printJSONLine(CaptureEvent(
                    type: "error",
                    source: "call",
                    path: nil,
                    elapsedSeconds: nil,
                    levelDb: nil,
                    message: "\(error)"
                ))
                Foundation.exit(1)
            }
        case "record-audio-tap":
            do {
                try recordAudioTap(parseRecordSystemOptions(Array(args.dropFirst())))
            } catch {
                try printJSONLine(CaptureEvent(
                    type: "error",
                    source: "call",
                    path: nil,
                    elapsedSeconds: nil,
                    levelDb: nil,
                    message: "\(error)"
                ))
                Foundation.exit(1)
            }
        case "probe-audio-tap":
            do {
                try probeAudioTap()
            } catch {
                try printJSONLine(CaptureEvent(
                    type: "error",
                    source: "call",
                    path: nil,
                    elapsedSeconds: nil,
                    levelDb: nil,
                    message: "\(error)"
                ))
                Foundation.exit(1)
            }
        case "version", "--version", "-V":
            print("recall-capture 0.1.0")
        case let command?:
            fputs("Unknown command: \(command)\n\n", stderr)
            printHelp(to: stderr)
            Foundation.exit(2)
        }
    }

    private static func listSources() -> SourceList {
        SourceList(
            type: "source_list",
            version: "0.1.0",
            generatedAtUnix: Int(Date().timeIntervalSince1970),
            candidates: appCandidates(),
            microphones: microphones(),
            permissions: Permissions(
                microphone: microphonePermission()
            )
        )
    }

    private static func parseRecordMicOptions(_ args: [String]) throws -> RecordMicOptions {
        var sessionDir: URL?
        var durationSeconds: TimeInterval = 8 * 60 * 60
        var stopFile: URL?
        var index = 0

        while index < args.count {
            let arg = args[index]
            switch arg {
            case "--session-dir":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                sessionDir = URL(fileURLWithPath: args[index + 1])
                index += 2
            case "--duration":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                let rawValue = args[index + 1]
                guard let parsed = TimeInterval(rawValue), parsed > 0 else {
                    throw CaptureError.invalidDuration(rawValue)
                }
                durationSeconds = parsed
                index += 2
            case "--stop-file":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                stopFile = URL(fileURLWithPath: args[index + 1])
                index += 2
            default:
                fputs("Ignoring unknown record-mic option: \(arg)\n", stderr)
                index += 1
            }
        }

        guard let sessionDir else {
            throw CaptureError.missingSessionDir
        }

        return RecordMicOptions(
            sessionDir: sessionDir,
            durationSeconds: durationSeconds,
            stopFile: stopFile
        )
    }

    private static func parseRecordSystemOptions(_ args: [String]) throws -> RecordSystemOptions {
        var sessionDir: URL?
        var durationSeconds: TimeInterval = 8 * 60 * 60
        var stopFile: URL?
        var index = 0

        while index < args.count {
            let arg = args[index]
            switch arg {
            case "--session-dir":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                sessionDir = URL(fileURLWithPath: args[index + 1])
                index += 2
            case "--duration":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                let rawValue = args[index + 1]
                guard let parsed = TimeInterval(rawValue), parsed > 0 else {
                    throw CaptureError.invalidDuration(rawValue)
                }
                durationSeconds = parsed
                index += 2
            case "--stop-file":
                guard index + 1 < args.count else {
                    throw CaptureError.missingValue(arg)
                }
                stopFile = URL(fileURLWithPath: args[index + 1])
                index += 2
            default:
                fputs("Ignoring unknown record-system option: \(arg)\n", stderr)
                index += 1
            }
        }

        guard let sessionDir else {
            throw CaptureError.missingSessionDir
        }

        return RecordSystemOptions(
            sessionDir: sessionDir,
            durationSeconds: durationSeconds,
            stopFile: stopFile
        )
    }

    private static func recordMic(_ options: RecordMicOptions) throws {
        try ensureMicrophonePermission()

        let audioDir = options.sessionDir.appendingPathComponent("audio", isDirectory: true)
        try FileManager.default.createDirectory(at: audioDir, withIntermediateDirectories: true)

        let outputURL = audioDir.appendingPathComponent("mic.m4a")
        if FileManager.default.fileExists(atPath: outputURL.path) {
            try FileManager.default.removeItem(at: outputURL)
        }

        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: 44_100,
            AVNumberOfChannelsKey: 1,
            AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue
        ]

        let recorder = try AVAudioRecorder(url: outputURL, settings: settings)
        recorder.isMeteringEnabled = true
        recorder.prepareToRecord()

        guard recorder.record() else {
            throw CaptureError.recorderCreationFailed
        }

        var currentInputDevice = defaultInputDevice()

        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_started",
            source: "mic",
            path: outputURL.path,
            elapsedSeconds: 0,
            levelDb: nil,
            message: currentInputDevice.map { "Mic input: \($0.name)" },
            deviceName: currentInputDevice?.name,
            deviceID: currentInputDevice?.uid
        ))

        let startedAt = Date()
        while Date().timeIntervalSince(startedAt) < options.durationSeconds
            && !shouldStopRecording(stopFile: options.stopFile)
        {
            Thread.sleep(forTimeInterval: 0.25)
            let latestInputDevice = defaultInputDevice()
            if latestInputDevice != currentInputDevice {
                currentInputDevice = latestInputDevice
                try printJSONLine(CaptureEvent(
                    type: "device_changed",
                    source: "mic",
                    path: nil,
                    elapsedSeconds: Date().timeIntervalSince(startedAt),
                    levelDb: nil,
                    message: latestInputDevice.map { "Mic input changed to \($0.name)" }
                        ?? "Mic input changed, but current default input could not be resolved.",
                    deviceName: latestInputDevice?.name,
                    deviceID: latestInputDevice?.uid
                ))
            }
            recorder.updateMeters()
            try printJSONLine(CaptureEvent(
                type: "level",
                source: "mic",
                path: nil,
                elapsedSeconds: Date().timeIntervalSince(startedAt),
                levelDb: recorder.averagePower(forChannel: 0),
                message: nil,
                deviceName: currentInputDevice?.name,
                deviceID: currentInputDevice?.uid
            ))
        }

        recorder.stop()

        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_stopped",
            source: "mic",
            path: outputURL.path,
            elapsedSeconds: Date().timeIntervalSince(startedAt),
            levelDb: nil,
            message: currentInputDevice.map { "Mic input at stop: \($0.name)" },
            deviceName: currentInputDevice?.name,
            deviceID: currentInputDevice?.uid
        ))
    }

    private static func defaultInputDevice() -> DefaultInputDevice? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var deviceID = AudioDeviceID(0)
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        let status = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject),
            &address,
            0,
            nil,
            &size,
            &deviceID
        )
        guard status == noErr, deviceID != AudioDeviceID(kAudioObjectUnknown) else {
            return nil
        }

        let name = audioObjectStringProperty(
            objectID: deviceID,
            selector: kAudioObjectPropertyName,
            scope: kAudioObjectPropertyScopeGlobal
        ) ?? "Unknown input"
        let uid = audioObjectStringProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyDeviceUID,
            scope: kAudioObjectPropertyScopeGlobal
        ) ?? "\(deviceID)"

        return DefaultInputDevice(id: deviceID, name: name, uid: uid)
    }

    private static func audioObjectStringProperty(
        objectID: AudioObjectID,
        selector: AudioObjectPropertySelector,
        scope: AudioObjectPropertyScope
    ) -> String? {
        var address = AudioObjectPropertyAddress(
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain
        )
        var value: CFString?
        var size = UInt32(MemoryLayout<CFString?>.size)
        let status = withUnsafeMutablePointer(to: &value) { pointer in
            AudioObjectGetPropertyData(
                objectID,
                &address,
                0,
                nil,
                &size,
                pointer
            )
        }
        guard status == noErr else {
            return nil
        }
        return value as String?
    }

    private static func recordSystem(_ options: RecordSystemOptions) async throws {
        let audioDir = options.sessionDir.appendingPathComponent("audio", isDirectory: true)
        try FileManager.default.createDirectory(at: audioDir, withIntermediateDirectories: true)

        let outputURL = audioDir.appendingPathComponent("call.m4a")
        if FileManager.default.fileExists(atPath: outputURL.path) {
            try FileManager.default.removeItem(at: outputURL)
        }

        let recorder = try SystemAudioRecorder(outputURL: outputURL)
        try await recorder.record(durationSeconds: options.durationSeconds, stopFile: options.stopFile)
    }

    private static func recordAudioTap(_ options: RecordSystemOptions) throws {
        guard #available(macOS 14.2, *) else {
            throw CaptureError.unsupportedOS("CoreAudio process taps require macOS 14.2 or newer")
        }

        let audioDir = options.sessionDir.appendingPathComponent("audio", isDirectory: true)
        try FileManager.default.createDirectory(at: audioDir, withIntermediateDirectories: true)

        let outputURL = audioDir.appendingPathComponent("call.m4a")
        if FileManager.default.fileExists(atPath: outputURL.path) {
            try FileManager.default.removeItem(at: outputURL)
        }

        let recorder = try CoreAudioTapRecorder(outputURL: outputURL)
        try recorder.record(durationSeconds: options.durationSeconds, stopFile: options.stopFile)
    }

    private static func probeAudioTap() throws {
        guard #available(macOS 14.2, *) else {
            throw CaptureError.unsupportedOS("CoreAudio process taps require macOS 14.2 or newer")
        }

        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        description.name = "Recall System Audio Probe"
        description.uuid = UUID()
        description.isPrivate = true
        description.muteBehavior = .unmuted

        var tapID = AudioObjectID(kAudioObjectUnknown)
        let status = AudioHardwareCreateProcessTap(description, &tapID)
        guard status == noErr else {
            throw CaptureError.processTapCreationFailed(status)
        }

        try printJSONLine(CaptureEvent(
            type: "audio_tap_probe_ok",
            source: "call",
            path: nil,
            elapsedSeconds: nil,
            levelDb: nil,
            message: "CoreAudio process tap created with id \(tapID)"
        ))

        let destroyStatus = AudioHardwareDestroyProcessTap(tapID)
        if destroyStatus == noErr {
            try printJSONLine(CaptureEvent(
                type: "audio_tap_probe_stopped",
                source: "call",
                path: nil,
                elapsedSeconds: nil,
                levelDb: nil,
                message: "CoreAudio process tap destroyed"
            ))
        } else {
            try printJSONLine(CaptureEvent(
                type: "warning",
                source: "call",
                path: nil,
                elapsedSeconds: nil,
                levelDb: nil,
                message: "Failed to destroy CoreAudio process tap: OSStatus \(destroyStatus)"
            ))
        }
    }

    fileprivate static func shouldStopRecording(stopFile: URL?) -> Bool {
        guard let stopFile else {
            return false
        }

        return FileManager.default.fileExists(atPath: stopFile.path)
    }

    private static func ensureMicrophonePermission() throws {
        let status = AVCaptureDevice.authorizationStatus(for: .audio)

        switch status {
        case .authorized:
            return
        case .denied:
            throw CaptureError.microphonePermissionDenied("denied")
        case .restricted:
            throw CaptureError.microphonePermissionDenied("restricted")
        case .notDetermined:
            let semaphore = DispatchSemaphore(value: 0)
            let result = PermissionResult()
            AVCaptureDevice.requestAccess(for: .audio) { allowed in
                result.granted = allowed
                semaphore.signal()
            }
            semaphore.wait()
            if !result.granted {
                throw CaptureError.microphonePermissionDenied("not_determined")
            }
        @unknown default:
            throw CaptureError.microphonePermissionDenied("unknown")
        }
    }

    private static func appCandidates() -> [AppCandidate] {
        NSWorkspace.shared.runningApplications
            .filter { app in
                guard app.activationPolicy == .regular else {
                    return false
                }
                return app.localizedName != nil
            }
            .map { app in
                let name = app.localizedName ?? "Unknown"
                let match = meetingMatch(name: name, bundleIdentifier: app.bundleIdentifier)
                return AppCandidate(
                    kind: "running_app",
                    name: name,
                    bundleIdentifier: app.bundleIdentifier,
                    processIdentifier: Int(app.processIdentifier),
                    confidence: match.confidence,
                    reason: match.reason
                )
            }
            .filter { candidate in
                candidate.confidence != "low" || isCommonBrowser(candidate.name)
            }
            .sorted { lhs, rhs in
                if score(lhs.confidence) == score(rhs.confidence) {
                    return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
                }
                return score(lhs.confidence) > score(rhs.confidence)
            }
    }

    private static func microphones() -> [AudioDevice] {
        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.microphone],
            mediaType: .audio,
            position: .unspecified
        )

        return discovery.devices.map { device in
            AudioDevice(
                kind: "microphone",
                name: device.localizedName,
                uniqueID: device.uniqueID
            )
        }
    }

    private static func meetingMatch(name: String, bundleIdentifier: String?) -> (confidence: String, reason: String) {
        let haystack = "\(name) \(bundleIdentifier ?? "")".lowercased()

        let highConfidenceTerms = [
            "microsoft teams",
            "zoom",
            "slack",
            "facetime",
            "webex",
            "discord",
            "google meet"
        ]

        if let term = highConfidenceTerms.first(where: { haystack.contains($0) }) {
            return ("high", "Matched meeting app term '\(term)'")
        }

        if isCommonBrowser(name) {
            return ("medium", "Browser may host Google Meet, Teams, Zoom, or Slack calls")
        }

        return ("low", "Running foreground-capable app")
    }

    private static func isCommonBrowser(_ name: String) -> Bool {
        let normalized = name.lowercased()
        return [
            "safari",
            "google chrome",
            "chrome",
            "firefox",
            "arc",
            "brave browser",
            "microsoft edge"
        ].contains { normalized.contains($0) }
    }

    private static func score(_ confidence: String) -> Int {
        switch confidence {
        case "high":
            3
        case "medium":
            2
        default:
            1
        }
    }

    private static func microphonePermission() -> String {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            "authorized"
        case .denied:
            "denied"
        case .restricted:
            "restricted"
        case .notDetermined:
            "not_determined"
        @unknown default:
            "unknown"
        }
    }

    private static func printJSON<T: Encodable>(_ value: T) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(value)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }

    fileprivate static func printJSONLine<T: Encodable>(_ value: T) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(value)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }

    private static func printHelp(to file: UnsafeMutablePointer<FILE> = stdout) {
        fputs(
            """
            recall-capture

            USAGE:
                recall-capture list-sources
                recall-capture record-mic --session-dir <path> [--duration <seconds>] [--stop-file <path>]
                recall-capture record-audio-tap --session-dir <path> [--duration <seconds>] [--stop-file <path>]
                recall-capture record-system --session-dir <path> [--duration <seconds>] [--stop-file <path>]
                recall-capture probe-audio-tap
                recall-capture version

            COMMANDS:
                list-sources    Emit candidate meeting apps and microphones as JSON.
                record-mic      Record default microphone audio into <session-dir>/audio/mic.m4a.
                record-audio-tap Record system audio with CoreAudio process taps into <session-dir>/audio/call.m4a.
                record-system   Record app/system audio into <session-dir>/audio/call.m4a.
                probe-audio-tap Probe CoreAudio's process-tap API without writing audio.
                version         Print helper version.

            NOTE:
                record-audio-tap is the preferred system-audio path. record-system uses
                ScreenCaptureKit and may require broader Screen Recording permission.

            """,
            file
        )
    }
}

@available(macOS 14.2, *)
final class CoreAudioTapRecorder: @unchecked Sendable {
    private let outputURL: URL
    private let writerQueue = DispatchQueue(label: "recall.core-audio-tap.writer")
    private let startedAt = Date()
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateDeviceID = AudioObjectID(kAudioObjectUnknown)
    private var ioProcID: AudioDeviceIOProcID?
    private var inputFormat: AVAudioFormat?
    private var audioFile: AVAudioFile?
    private var lastLevelEventAt = Date.distantPast
    private var writeError: Error?

    init(outputURL: URL) throws {
        self.outputURL = outputURL
    }

    func record(durationSeconds: TimeInterval, stopFile: URL?) throws {
        try start()
        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_started",
            source: "call",
            path: outputURL.path,
            elapsedSeconds: 0,
            levelDb: nil,
            message: "CoreAudio process tap recording started"
        ))

        while Date().timeIntervalSince(startedAt) < durationSeconds
            && !RecallCapture.shouldStopRecording(stopFile: stopFile)
        {
            Thread.sleep(forTimeInterval: 0.25)
            if let writeError {
                throw writeError
            }
        }

        stop()

        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_stopped",
            source: "call",
            path: outputURL.path,
            elapsedSeconds: Date().timeIntervalSince(startedAt),
            levelDb: nil,
            message: "CoreAudio process tap recording stopped"
        ))
    }

    private func start() throws {
        let description = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        description.name = "Recall System Audio"
        description.uuid = UUID()
        description.isPrivate = true
        description.muteBehavior = .unmuted

        var newTapID = AudioObjectID(kAudioObjectUnknown)
        let tapStatus = AudioHardwareCreateProcessTap(description, &newTapID)
        guard tapStatus == noErr else {
            throw CaptureError.processTapCreationFailed(tapStatus)
        }
        tapID = newTapID

        let tapUID = try readAudioObjectString(
            objectID: tapID,
            selector: kAudioTapPropertyUID,
            name: "kAudioTapPropertyUID"
        )
        var streamDescription = try readAudioStreamDescription(
            objectID: tapID,
            selector: kAudioTapPropertyFormat,
            name: "kAudioTapPropertyFormat"
        )

        guard let format = AVAudioFormat(streamDescription: &streamDescription) else {
            throw CaptureError.audioFormatUnavailable
        }
        inputFormat = format
        audioFile = try AVAudioFile(
            forWriting: outputURL,
            settings: [
                AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
                AVSampleRateKey: format.sampleRate,
                AVNumberOfChannelsKey: Int(format.channelCount),
                AVEncoderBitRateKey: 128_000
            ],
            commonFormat: format.commonFormat,
            interleaved: format.isInterleaved
        )

        let aggregateUID = "com.recall.audio-tap.\(UUID().uuidString)"
        let aggregateDescription: [String: Any] = [
            "name": "Recall System Audio",
            "uid": aggregateUID,
            "private": true,
            "tapautostart": false,
            "taps": [
                [
                    "uid": tapUID,
                    "drift": true
                ]
            ]
        ]

        var newAggregateDeviceID = AudioObjectID(kAudioObjectUnknown)
        let aggregateStatus = AudioHardwareCreateAggregateDevice(
            aggregateDescription as CFDictionary,
            &newAggregateDeviceID
        )
        guard aggregateStatus == noErr else {
            throw CaptureError.aggregateDeviceCreationFailed(aggregateStatus)
        }
        aggregateDeviceID = newAggregateDeviceID

        var newIOProcID: AudioDeviceIOProcID?
        let ioStatus = AudioDeviceCreateIOProcID(
            aggregateDeviceID,
            coreAudioTapIOProc,
            Unmanaged.passUnretained(self).toOpaque(),
            &newIOProcID
        )
        guard ioStatus == noErr, let newIOProcID else {
            throw CaptureError.audioDeviceIOProcCreationFailed(ioStatus)
        }
        ioProcID = newIOProcID

        let startStatus = AudioDeviceStart(aggregateDeviceID, newIOProcID)
        guard startStatus == noErr else {
            throw CaptureError.audioDeviceStartFailed(startStatus)
        }
    }

    private func stop() {
        if aggregateDeviceID != kAudioObjectUnknown, let ioProcID {
            let _ = AudioDeviceStop(aggregateDeviceID, ioProcID)
            let _ = AudioDeviceDestroyIOProcID(aggregateDeviceID, ioProcID)
            self.ioProcID = nil
        }

        writerQueue.sync {
            self.audioFile = nil
        }

        if aggregateDeviceID != kAudioObjectUnknown {
            let _ = AudioHardwareDestroyAggregateDevice(aggregateDeviceID)
            aggregateDeviceID = AudioObjectID(kAudioObjectUnknown)
        }

        if tapID != kAudioObjectUnknown {
            let _ = AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    fileprivate func handleInput(_ inputData: UnsafePointer<AudioBufferList>?) -> OSStatus {
        guard let inputData,
              let inputFormat,
              let buffer = makePCMBuffer(from: inputData, format: inputFormat)
        else {
            return noErr
        }

        writerQueue.async {
            do {
                try self.audioFile?.write(from: buffer)
                self.maybeEmitLevel(buffer)
            } catch {
                self.writeError = error
            }
        }

        return noErr
    }

    private func makePCMBuffer(
        from inputData: UnsafePointer<AudioBufferList>,
        format: AVAudioFormat
    ) -> AVAudioPCMBuffer? {
        let sourceBuffers = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inputData)
        )
        guard let firstBuffer = sourceBuffers.first else {
            return nil
        }

        let bytesPerFrame = max(Int(format.streamDescription.pointee.mBytesPerFrame), 1)
        let frameCapacity = AVAudioFrameCount(Int(firstBuffer.mDataByteSize) / bytesPerFrame)
        guard frameCapacity > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCapacity)
        else {
            return nil
        }

        buffer.frameLength = frameCapacity
        let destinationBuffers = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
        let count = min(sourceBuffers.count, destinationBuffers.count)

        for index in 0..<count {
            let source = sourceBuffers[index]
            var destination = destinationBuffers[index]
            guard let sourceData = source.mData,
                  let destinationData = destination.mData
            else {
                continue
            }

            let byteCount = min(Int(source.mDataByteSize), Int(destination.mDataByteSize))
            memcpy(destinationData, sourceData, byteCount)
            destination.mDataByteSize = UInt32(byteCount)
            destinationBuffers[index] = destination
        }

        return buffer
    }

    private func maybeEmitLevel(_ buffer: AVAudioPCMBuffer) {
        let now = Date()
        guard now.timeIntervalSince(lastLevelEventAt) >= 0.25 else {
            return
        }
        lastLevelEventAt = now

        let level = audioLevelDb(buffer)
        try? RecallCapture.printJSONLine(CaptureEvent(
            type: "level",
            source: "call",
            path: nil,
            elapsedSeconds: now.timeIntervalSince(startedAt),
            levelDb: level,
            message: nil
        ))
    }

    private func audioLevelDb(_ buffer: AVAudioPCMBuffer) -> Float? {
        let channelCount = Int(buffer.format.channelCount)
        let frameLength = Int(buffer.frameLength)
        guard frameLength > 0 else {
            return nil
        }

        var sumSquares = 0.0
        var sampleCount = 0

        if let floatChannelData = buffer.floatChannelData {
            for channel in 0..<channelCount {
                let samples = floatChannelData[channel]
                for frame in 0..<frameLength {
                    let value = Double(samples[frame])
                    sumSquares += value * value
                    sampleCount += 1
                }
            }
        } else if let int16ChannelData = buffer.int16ChannelData {
            for channel in 0..<channelCount {
                let samples = int16ChannelData[channel]
                for frame in 0..<frameLength {
                    let value = Double(samples[frame]) / Double(Int16.max)
                    sumSquares += value * value
                    sampleCount += 1
                }
            }
        }

        guard sampleCount > 0, sumSquares > 0 else {
            return -60
        }

        let rms = sqrt(sumSquares / Double(sampleCount))
        return max(20 * Float(log10(rms)), -60)
    }
}

@available(macOS 14.2, *)
private let coreAudioTapIOProc: AudioDeviceIOProc = {
    _, _, inputData, _, _, _, clientData in
    guard let clientData else {
        return noErr
    }

    let recorder = Unmanaged<CoreAudioTapRecorder>
        .fromOpaque(clientData)
        .takeUnretainedValue()
    return recorder.handleInput(inputData)
}

private func readAudioObjectString(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    name: String
) throws -> String {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    let pointer = UnsafeMutableRawPointer.allocate(
        byteCount: MemoryLayout<CFString?>.size,
        alignment: MemoryLayout<CFString?>.alignment
    )
    pointer.initializeMemory(as: CFString?.self, repeating: nil, count: 1)
    defer {
        pointer.assumingMemoryBound(to: CFString?.self).deinitialize(count: 1)
        pointer.deallocate()
    }

    var size = UInt32(MemoryLayout<CFString?>.size)
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, pointer)
    guard status == noErr,
          let value = pointer.load(as: CFString?.self)
    else {
        throw CaptureError.audioObjectPropertyFailed(name, status)
    }

    return value as String
}

private func readAudioStreamDescription(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    name: String
) throws -> AudioStreamBasicDescription {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var value = AudioStreamBasicDescription()
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, &value)
    guard status == noErr else {
        throw CaptureError.audioObjectPropertyFailed(name, status)
    }

    return value
}

final class SystemAudioRecorder: NSObject, SCStreamOutput, @unchecked Sendable {
    private let outputURL: URL
    private let writer: AVAssetWriter
    private let audioInput: AVAssetWriterInput
    private let sampleQueue = DispatchQueue(label: "recall.system-audio.samples")
    private let lock = NSLock()
    private var startedAt: Date?
    private var didStartWriting = false
    private var lastLevelEventAt = Date.distantPast

    init(outputURL: URL) throws {
        self.outputURL = outputURL
        self.writer = try AVAssetWriter(outputURL: outputURL, fileType: .m4a)
        self.audioInput = AVAssetWriterInput(
            mediaType: .audio,
            outputSettings: [
                AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
                AVSampleRateKey: 44_100,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 128_000
            ]
        )
        self.audioInput.expectsMediaDataInRealTime = true

        super.init()

        guard writer.canAdd(audioInput) else {
            throw CaptureError.assetWriterUnavailable
        }
        writer.add(audioInput)
    }

    func record(durationSeconds: TimeInterval, stopFile: URL?) async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )

        guard let display = content.displays.first else {
            throw CaptureError.noShareableDisplays
        }

        let filter = SCContentFilter(display: display, excludingWindows: [])
        let configuration = SCStreamConfiguration()
        configuration.width = 2
        configuration.height = 2
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        configuration.capturesAudio = true
        configuration.excludesCurrentProcessAudio = true
        configuration.sampleRate = 44_100
        configuration.channelCount = 2

        let stream = SCStream(filter: filter, configuration: configuration, delegate: nil)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: sampleQueue)

        startedAt = Date()
        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_started",
            source: "call",
            path: outputURL.path,
            elapsedSeconds: 0,
            levelDb: nil,
            message: nil
        ))

        try await stream.startCapture()

        while Date().timeIntervalSince(startedAt ?? Date()) < durationSeconds
            && !RecallCapture.shouldStopRecording(stopFile: stopFile)
        {
            try await Task.sleep(nanoseconds: 250_000_000)
        }

        try await stream.stopCapture()
        finishWriting()

        try RecallCapture.printJSONLine(CaptureEvent(
            type: "recording_stopped",
            source: "call",
            path: outputURL.path,
            elapsedSeconds: Date().timeIntervalSince(startedAt ?? Date()),
            levelDb: nil,
            message: nil
        ))
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .audio,
              sampleBuffer.isValid,
              CMSampleBufferDataIsReady(sampleBuffer)
        else {
            return
        }

        lock.lock()
        defer { lock.unlock() }

        if !didStartWriting {
            let presentationTime = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
            guard writer.startWriting() else {
                return
            }
            writer.startSession(atSourceTime: presentationTime)
            didStartWriting = true
        }

        if audioInput.isReadyForMoreMediaData {
            audioInput.append(sampleBuffer)
        }

        let now = Date()
        if now.timeIntervalSince(lastLevelEventAt) >= 0.25 {
            lastLevelEventAt = now
            let elapsed = now.timeIntervalSince(startedAt ?? now)
            let level = audioLevelDb(sampleBuffer)
            try? RecallCapture.printJSONLine(CaptureEvent(
                type: "level",
                source: "call",
                path: nil,
                elapsedSeconds: elapsed,
                levelDb: level,
                message: nil
            ))
        }
    }

    private func finishWriting() {
        lock.lock()
        defer { lock.unlock() }

        guard didStartWriting else {
            writer.cancelWriting()
            return
        }

        audioInput.markAsFinished()
        let semaphore = DispatchSemaphore(value: 0)
        writer.finishWriting {
            semaphore.signal()
        }
        semaphore.wait()
    }

    private func audioLevelDb(_ sampleBuffer: CMSampleBuffer) -> Float? {
        guard let formatDescription = CMSampleBufferGetFormatDescription(sampleBuffer),
              let streamDescription = CMAudioFormatDescriptionGetStreamBasicDescription(formatDescription)
        else {
            return nil
        }

        let asbd = streamDescription.pointee
        guard asbd.mFormatID == kAudioFormatLinearPCM,
              asbd.mBitsPerChannel == 32,
              asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
        else {
            return nil
        }

        let frameCount = CMSampleBufferGetNumSamples(sampleBuffer)
        let channelCount = max(Int(asbd.mChannelsPerFrame), 1)
        let byteCount = frameCount * channelCount * MemoryLayout<Float>.size
        guard byteCount > 0 else {
            return nil
        }

        var pcmData = Data(count: byteCount)
        let status = pcmData.withUnsafeMutableBytes { rawBuffer in
            guard let baseAddress = rawBuffer.baseAddress else {
                return noErr
            }

            let audioBuffer = AudioBuffer(
                mNumberChannels: asbd.mChannelsPerFrame,
                mDataByteSize: UInt32(byteCount),
                mData: baseAddress
            )
            var audioBufferList = AudioBufferList(mNumberBuffers: 1, mBuffers: audioBuffer)
            return CMSampleBufferCopyPCMDataIntoAudioBufferList(
                sampleBuffer,
                at: 0,
                frameCount: Int32(frameCount),
                into: &audioBufferList
            )
        }

        guard status == noErr else {
            return nil
        }

        let sumSquares = pcmData.withUnsafeBytes { rawBuffer -> Double in
            let samples = rawBuffer.bindMemory(to: Float.self)
            guard !samples.isEmpty else {
                return 0
            }
            return samples.reduce(0) { partial, sample in
                let value = Double(sample)
                return partial + value * value
            } / Double(samples.count)
        }

        guard sumSquares > 0 else {
            return -60
        }

        return max(20 * Float(log10(sqrt(sumSquares))), -60)
    }
}
