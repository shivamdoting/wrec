import VideoToolbox
import CoreGraphics
var list: CFArray?
VTCopyVideoEncoderList(nil, &list)
for case let e as [String: Any] in (list as? [Any]) ?? [] {
  let name = e[kVTVideoEncoderList_EncoderName as String] ?? "?"
  let hw = e[kVTVideoEncoderList_IsHardwareAccelerated as String] ?? false
  print("encoder \(name) hw=\(hw)")
}
print("CGPreflightScreenCaptureAccess=\(CGPreflightScreenCaptureAccess())")
let ids = (0..<16).map { _ in CGDirectDisplayID(0) }
var d = ids; var n: UInt32 = 0
CGGetActiveDisplayList(16, &d, &n)
for i in 0..<Int(n) { let id = d[i]; print("display \(id) \(CGDisplayPixelsWide(id))x\(CGDisplayPixelsHigh(id)) refresh=\(CGDisplayCopyDisplayMode(id)?.refreshRate ?? -1)") }
