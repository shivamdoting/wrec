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
        guard let context = NSGraphicsContext.current?.cgContext else {
            return
        }

        // Keep the source visibly changing without making the stimulus itself
        // a CPU/GPU benchmark. The old 32×18 AppKit checkerboard allocated and
        // filled hundreds of colored rectangles per frame, which competed with
        // the recorder and made otherwise identical reps diverge.
        let phase = CGFloat(frameIndex % 240) / 240
        context.setFillColor(
            red: 0.08 + phase * 0.12,
            green: 0.12,
            blue: 0.2 + (1 - phase) * 0.12,
            alpha: 1
        )
        context.fill(bounds)

        let bandWidth = bounds.width / 8
        for band in 0..<8 {
            let value = CGFloat((Int(frameIndex) + band * 31) % 255) / 255
            context.setFillColor(red: value, green: 0.65, blue: 1 - value, alpha: 1)
            context.fill(
                CGRect(
                    x: CGFloat(band) * bandWidth,
                    y: 96,
                    width: bandWidth,
                    height: 360
                )
            )
        }

        let sweepX = CGFloat(frameIndex % UInt32(canvasWidth))
        context.setFillColor(gray: 1, alpha: 0.8)
        context.fill(CGRect(x: sweepX, y: 0, width: 20, height: bounds.height))
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

// An idle machine dims the display mid-run; the display link then pauses,
// frames stop, and the recorder gets blamed for a dark screen. Hold the
// display awake and opt out of App Nap for the stimulus lifetime.
let activity = ProcessInfo.processInfo.beginActivity(
    options: [.userInitiated, .idleDisplaySleepDisabled, .idleSystemSleepDisabled],
    reason: "wrec bench stimulus"
)
_ = activity

let view = StimulusView(frame: NSRect(x: 0, y: 0, width: canvasWidth, height: canvasHeight))
guard let screen = NSScreen.main else {
    fputs("stimulus: no screen available\n", stderr)
    exit(1)
}

let visible = screen.visibleFrame
// A single point stays on the physical display so WindowServer keeps
// compositing the full backing surface for ScreenCaptureKit. The remaining
// 1279×720 points sit beyond the right edge, so the benchmark never flashes
// over the user's work or steals focus.
let origin = NSPoint(x: visible.maxX - 1, y: visible.minY)
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
window.ignoresMouseEvents = true
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
