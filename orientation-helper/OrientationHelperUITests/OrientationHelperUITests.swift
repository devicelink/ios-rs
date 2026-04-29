import XCTest

/// A short-lived XCUITest run by ios-rs to set the device screen orientation.
///
/// ios-rs passes the desired orientation via the ORIENTATION environment variable:
///   portrait | portrait_upside_down | landscape_left | landscape_right
///
/// Usage (via ios-rs CLI):
///   ios orientation set landscape_left \
///     --runner-bundle-id it.luedeke.devicelink.ios-rs-helper.xctrunner \
///     --xctest-config ios-rs-helperUITests.xctest
class OrientationHelperTests: XCTestCase {

    func testSetOrientation() throws {
        let raw = ProcessInfo.processInfo.environment["ORIENTATION"] ?? "portrait"
        XCUIDevice.shared.orientation = deviceOrientation(from: raw)
        // Brief pause to let SpringBoard settle before the runner exits.
        Thread.sleep(forTimeInterval: 0.3)
    }

    private func deviceOrientation(from string: String) -> UIDeviceOrientation {
        switch string.lowercased() {
        case "portrait_upside_down": return .portraitUpsideDown
        case "landscape_left":       return .landscapeLeft
        case "landscape_right":      return .landscapeRight
        default:                     return .portrait
        }
    }
}
