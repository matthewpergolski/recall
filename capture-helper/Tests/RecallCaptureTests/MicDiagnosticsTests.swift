import XCTest
import AVFoundation
@testable import RecallCapture

final class MicDiagnosticsTests: XCTestCase {
    func testRawEnergyAndZeros() {
        var window = MicInputDiagnostics.Window()
        [Float(0), 0.5, -0.5, 0].withUnsafeBufferPointer { window.add($0) }
        XCTAssertEqual(window.samples, 4)
        XCTAssertEqual(window.zeros, 2)
        XCTAssertEqual(window.peak, 0.5)
        XCTAssertEqual(window.rmsDb!, 10 * log10(0.125), accuracy: 0.00001)
    }

    func testSilenceAndInvalidSamplesAreNotReportedAsSpeech() {
        var window = MicInputDiagnostics.Window()
        [Float(0), .nan, .infinity].withUnsafeBufferPointer { window.add($0) }
        XCTAssertEqual(window.nonfinite, 2)
        XCTAssertNil(window.rmsDb)
    }

    func testMissingBuffersStillProduceDiagnostics() {
        let diagnostics = MicInputDiagnostics()
        let result = diagnostics.observe(nil, format: nil, copiedBuffer: nil,
                                         muted: false, elapsed: 1.1)
        XCTAssertEqual(result?.missingBuffers, 1)
        XCTAssertEqual(result?.copiedFrames, 0)
        XCTAssertNil(result?.rmsDb)
    }

    func testRawAndCopiedMeasurementsPreserveQuietInputBelowMeterFloor() {
        let format = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 1)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4)!
        buffer.frameLength = 4
        for index in 0..<4 { buffer.floatChannelData![0][index] = 0.0001 }
        let result = MicInputDiagnostics().observe(buffer.audioBufferList, format: format,
                                                   copiedBuffer: buffer, muted: false, elapsed: 1.1)!
        XCTAssertEqual(result.rmsDb!, -80, accuracy: 0.001)
        XCTAssertEqual(result.copiedSamples, 4)
        XCTAssertEqual(result.energy, result.copiedEnergy, accuracy: 1e-12)
    }
}
