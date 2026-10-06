import XCTest
@testable import RecallCapture

final class AppleSpeechAssetsTests: XCTestCase {
    func testNoPendingRequestIsReady() {
        XCTAssertTrue(RecallCapture.appleSpeechAssetsReady(
            requestPending: false, locale: "en-US", installedLocales: []))
    }

    func testEmptyPendingRequestForInstalledLocaleIsReady() {
        XCTAssertTrue(RecallCapture.appleSpeechAssetsReady(
            requestPending: true, locale: "en-US", installedLocales: ["en-GB", "en-US"]))
    }

    func testPendingRequestForMissingLocaleIsNotReady() {
        XCTAssertFalse(RecallCapture.appleSpeechAssetsReady(
            requestPending: true, locale: "en-US", installedLocales: ["en-GB"]))
    }
}
