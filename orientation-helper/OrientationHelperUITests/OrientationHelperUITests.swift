import XCTest

/// Sets device orientation via SpringBoard private API — no UI testing init needed,
/// so the runner stays in the background without disrupting the foreground app.
class OrientationHelperTests: XCTestCase {

    func testSetOrientation() throws {
        let raw = ProcessInfo.processInfo.environment["ORIENTATION"] ?? "portrait"
        let orientation = intOrientation(from: raw)

        // Try SBSSetFrontMostDisplayOrientationOverride via SpringBoardServices
        typealias SetOrientation = @convention(c) (Int32) -> Void
        if let handle = dlopen("/System/Library/PrivateFrameworks/SpringBoardServices.framework/SpringBoardServices", RTLD_NOW),
           let sym = dlsym(handle, "SBSSetFrontMostDisplayOrientationOverride") {
            let fn = unsafeBitCast(sym, to: SetOrientation.self)
            fn(orientation)
            Thread.sleep(forTimeInterval: 0.3)
            dlclose(handle)
            return
        }

        // Fallback: XCUIDevice (requires initializeForUITesting=true)
        XCUIDevice.shared.orientation = xcuiOrientation(from: raw)
        Thread.sleep(forTimeInterval: 0.3)
    }

    // UIInterfaceOrientation values
    private func intOrientation(from s: String) -> Int32 {
        switch s.lowercased() {
        case "portrait_upside_down": return 2
        case "landscape_left":       return 4  // home button on left
        case "landscape_right":      return 3  // home button on right
        default:                     return 1  // portrait
        }
    }

    private func xcuiOrientation(from s: String) -> UIDeviceOrientation {
        switch s.lowercased() {
        case "portrait_upside_down": return .portraitUpsideDown
        case "landscape_left":       return .landscapeLeft
        case "landscape_right":      return .landscapeRight
        default:                     return .portrait
        }
    }
}
