import AppKit
import Foundation

let stimulusTitle = "wrec-bench-stimulus"
let canvasWidth: CGFloat = 1280
let canvasHeight: CGFloat = 720
let markerBlockSize: CGFloat = 24
let markerBlockHeight: CGFloat = 64
let markerBitCount = 32
let markerGuardCount = 4
let markerY: CGFloat = 632

final class StimulusView: NSView {
    private var frameIndex: UInt32 = 0

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        guard let context = NSGraphicsContext.current?.cgContext else {
            return
        }

        // The index advances per actual render, not per tick: every index that
        // reaches the screen is consecutive, so a missing index in a recording
        // is exactly one displayed frame the recorder failed to capture.
        frameIndex &+= 1
        context.setShouldAntialias(false)
        drawAnimatedField()
        drawMarkerStrip()
    }

    private func drawAnimatedField() {
        let cellSize: CGFloat = 40
        let phase = Int(frameIndex / 2)
        let hue = CGFloat(frameIndex % 360) / 360

        NSColor(calibratedHue: hue, saturation: 0.65, brightness: 0.92, alpha: 1).setFill()
        bounds.fill()

        for y in stride(from: CGFloat(0), to: bounds.height, by: cellSize) {
            for x in stride(from: CGFloat(0), to: bounds.width, by: cellSize) {
                let xi = Int(x / cellSize)
                let yi = Int(y / cellSize)
                let bright = (xi + yi + phase) % 2 == 0
                let localHue = CGFloat((xi * 17 + yi * 29 + phase * 3) % 360) / 360
                let color = NSColor(
                    calibratedHue: localHue,
                    saturation: bright ? 0.72 : 0.48,
                    brightness: bright ? 0.98 : 0.35,
                    alpha: 1
                )
                color.setFill()
                NSRect(x: x, y: y, width: cellSize, height: cellSize).fill()
            }
        }

        let sweepX = CGFloat(frameIndex % 1280)
        NSColor(calibratedWhite: 1, alpha: 0.24).setFill()
        NSRect(x: sweepX, y: 0, width: 18, height: bounds.height).fill()
    }

    private func drawMarkerStrip() {
        let totalBlocks = markerBitCount + markerGuardCount * 2
        let startX = (bounds.width - CGFloat(totalBlocks) * markerBlockSize) / 2
        let guardPrefix = [true, false, true, false]
        let guardSuffix = [false, true, false, true]

        NSColor(calibratedWhite: 0.5, alpha: 1).setFill()
        NSRect(
            x: startX - 8,
            y: markerY - 8,
            width: CGFloat(totalBlocks) * markerBlockSize + 16,
            height: markerBlockHeight + 16
        ).fill()

        for block in 0..<totalBlocks {
            let isWhite: Bool
            if block < markerGuardCount {
                isWhite = guardPrefix[block]
            } else if block >= markerGuardCount + markerBitCount {
                isWhite = guardSuffix[block - markerGuardCount - markerBitCount]
            } else {
                let bit = block - markerGuardCount
                isWhite = ((frameIndex >> UInt32(bit)) & 1) == 1
            }

            (isWhite ? NSColor.white : NSColor.black).setFill()
            NSRect(
                x: startX + CGFloat(block) * markerBlockSize,
                y: markerY,
                width: markerBlockSize,
                height: markerBlockHeight
            ).fill()
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
app.finishLaunching()

let view = StimulusView(frame: NSRect(x: 0, y: 0, width: canvasWidth, height: canvasHeight))
guard let screen = NSScreen.main else {
    fputs("stimulus: no screen available\n", stderr)
    exit(1)
}

let visible = screen.visibleFrame
let origin = NSPoint(
    x: min(max(visible.minX + 24, visible.minX), visible.maxX - canvasWidth),
    y: min(max(visible.maxY - canvasHeight - 48, visible.minY), visible.maxY - canvasHeight)
)
let window = NSWindow(
    contentRect: NSRect(origin: origin, size: NSSize(width: canvasWidth, height: canvasHeight)),
    styleMask: [.borderless],
    backing: .buffered,
    defer: false
)
window.title = stimulusTitle
window.contentView = view
// wrec only lists windows on layer 0 (the normal app-window layer), so the
// stimulus must not float — SCK captures the target window even if occluded.
window.level = .normal
window.isOpaque = true
window.backgroundColor = .black
window.hasShadow = false
window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
window.isReleasedWhenClosed = false
window.orderFrontRegardless()

// A Timer at 1/60 s gets coalesced to ~58 fps by the runloop, which starves a
// 60 fps capture of frames it then gets blamed for missing. A display link
// fires in sync with the panel refresh instead.
final class Ticker: NSObject {
    @objc func tick(_ link: CADisplayLink) {
        view.needsDisplay = true
    }
}

let ticker = Ticker()
let displayLink = view.displayLink(target: ticker, selector: #selector(Ticker.tick(_:)))
displayLink.add(to: .main, forMode: .common)

let backingSize = view.convertToBacking(view.bounds).size
print(
    "STIMULUS_READY title=\(stimulusTitle) points=\(Int(canvasWidth))x\(Int(canvasHeight)) pixels=\(Int(backingSize.width.rounded()))x\(Int(backingSize.height.rounded())) scale=\(String(format: "%.2f", window.backingScaleFactor))"
)
fflush(stdout)

RunLoop.main.run()
