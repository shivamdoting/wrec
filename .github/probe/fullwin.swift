import AppKit
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let win = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 960, height: 600), styleMask: [.titled], backing: .buffered, defer: false)
win.title = "wrec-probe-full"
let view = NSView(frame: win.contentView!.bounds)
view.wantsLayer = true
win.contentView = view
win.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
var n = 0
Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { _ in
  n += 1
  let h = CGFloat(n % 360) / 360.0
  view.layer?.backgroundColor = NSColor(hue: h, saturation: 1, brightness: 1, alpha: 1).cgColor
}
print("READY"); fflush(stdout)
app.run()
