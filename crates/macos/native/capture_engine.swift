import Foundation
import AppKit
import ScreenCaptureKit
import AVFoundation
import AudioToolbox
import CoreGraphics
import Darwin
import CoreMedia
import CoreVideo

// Channel contract with the Rust parent (crates/macos/src/lib.rs):
// - stdout carries machine-readable data only. In recording mode that is one
//   JSON event per line (`started`, `failed`); in --list mode TSV rows; in
//   permission modes a single status token.
// - stderr is human diagnostics only and is never parsed for state.

func logLine(_ message: String) {
    FileHandle.standardError.write(Data("capture-engine: \(message)\n".utf8))
}

// Serializes stdout writes: events are emitted from the recorder queue, the
// main actor, and ScreenCaptureKit delegate queues, and interleaved bytes
// would corrupt the parent's line-based parser.
let eventQueue = DispatchQueue(label: "wrec.capture.events")

// Keep event names and fields in sync with EngineEvent in crates/macos/src/lib.rs.
// Synchronous so an event is fully written before a caller exits the process.
func emitEvent(_ payload: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: payload) else {
        return
    }
    data.append(0x0A)
    eventQueue.sync {
        FileHandle.standardOutput.write(data)
    }
}

func emitFailure(_ message: String) {
    logLine(message)
    emitEvent(["event": "failed", "message": message])
}

/// Always-on-top "Mic on" pill shown while a recording captures the
/// microphone, so recordings started out of sight (CLI, agents) are never
/// silently listening. The panel dies with the process, so a crashed engine
/// cannot leave a stale indicator.
@MainActor
final class MicIndicator {
    private var panel: NSPanel?

    // Automated-verification hook. "1" forces the pill without microphone
    // capture (production sharing behavior); "capturable" additionally skips
    // the capture exclusion so screenshots can see what eyes see.
    static let testHook = ProcessInfo.processInfo.environment["WREC_MIC_PILL_TEST"]
    static let testHookEnabled = testHook == "1" || testHook == "capturable"

    static let pillWidth: CGFloat = 96
    static let pillHeight: CGFloat = 30
    static let bottomMargin: CGFloat = 24

    func show(on screen: NSScreen?) {
        guard panel == nil else {
            return
        }
        guard let screen = screen ?? NSScreen.main else {
            logLine("mic indicator unavailable: no screen")
            return
        }
        // .prohibited cannot create windows; .accessory still has no Dock
        // icon or menu bar. This process never calls NSApplication.run(), so
        // launching must be finished explicitly before windows can appear.
        NSApplication.shared.setActivationPolicy(.accessory)
        NSApplication.shared.finishLaunching()

        let frame = screen.visibleFrame
        let origin = NSPoint(
            x: frame.midX - Self.pillWidth / 2,
            y: frame.minY + Self.bottomMargin
        )
        let panel = NSPanel(
            contentRect: NSRect(origin: origin, size: NSSize(width: Self.pillWidth, height: Self.pillHeight)),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = .statusBar
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        // Excluded from capture at the window-server level, so the pill stays
        // out of the recording even if the shareable-content snapshot races
        // window registration and misses it (the exclusion-list fallback
        // alone is racy; independent QA caught it leaking a frame).
        if Self.testHook != "capturable" {
            panel.sharingType = .none
        }
        panel.contentView = Self.pillView()
        panel.orderFrontRegardless()
        self.panel = panel
        logLine("mic indicator shown windowNumber=\(panel.windowNumber)")
    }

    func hide() {
        panel?.orderOut(nil)
        panel = nil
    }

    private static func pillView() -> NSView {
        let view = NSView(frame: NSRect(x: 0, y: 0, width: pillWidth, height: pillHeight))
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor(white: 0.07, alpha: 0.9).cgColor
        view.layer?.cornerRadius = pillHeight / 2
        view.layer?.borderWidth = 1
        view.layer?.borderColor = NSColor(white: 1, alpha: 0.15).cgColor

        let icon = NSImageView(frame: NSRect(x: 12, y: (pillHeight - 16) / 2, width: 16, height: 16))
        icon.image = NSImage(systemSymbolName: "waveform", accessibilityDescription: "Microphone recording")
        icon.contentTintColor = NSColor(red: 0.9, green: 0.28, blue: 0.3, alpha: 1)
        view.addSubview(icon)

        let label = NSTextField(labelWithString: "Mic on")
        label.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        label.textColor = .white
        label.sizeToFit()
        label.frame.origin = NSPoint(x: 36, y: (pillHeight - label.frame.height) / 2)
        view.addSubview(label)

        return view
    }
}

func screenForDisplay(_ displayID: UInt32) -> NSScreen? {
    NSScreen.screens.first { screen in
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        return (screen.deviceDescription[key] as? NSNumber)?.uint32Value == displayID
    }
}

// SCWindow frames are top-left-origin global coordinates; NSScreen frames are
// bottom-left-origin. Match by the window's midpoint after flipping.
func screenContaining(windowFrame: CGRect) -> NSScreen? {
    guard let primary = NSScreen.screens.first else {
        return nil
    }
    let midpoint = NSPoint(
        x: windowFrame.midX,
        y: primary.frame.maxY - windowFrame.midY
    )
    return NSScreen.screens.first { $0.frame.contains(midpoint) }
}

// Exit codes follow BSD sysexits(3). The Rust parent branches on these; keep
// in sync with the EX_* constants in crates/macos/src/lib.rs.
enum Exit {
    static let usage: Int32 = 64        // EX_USAGE: bad arguments
    static let unavailable: Int32 = 69  // EX_UNAVAILABLE: capture target missing
    static let software: Int32 = 70     // EX_SOFTWARE: internal error
    static let ioError: Int32 = 74      // EX_IOERR: writer/finalization failure
    static let noPermission: Int32 = 77 // EX_NOPERM: TCC permission missing
}

final class SampleRecorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let queue = DispatchQueue(label: "wrec.capture.writer", qos: .userInitiated)

    private let writer: AVAssetWriter
    private let videoInput: AVAssetWriterInput
    private let audioInput: AVAssetWriterInput?
    private let micInput: AVAssetWriterInput?
    private let finished = DispatchSemaphore(value: 0)
    private var didStart = false
    private var didFinish = false
    private var frameCount: Int64 = 0
    private var droppedFrameCount: Int64 = 0
    private var audioSampleCount: Int64 = 0
    private var droppedAudioSampleCount: Int64 = 0
    private var micSampleCount: Int64 = 0
    private var droppedMicSampleCount: Int64 = 0
    private var firstPTS: CMTime?
    private var isPaused = false
    private var pauseStartedPTS: CMTime?
    private var pendingResume = false
    private var pauseOffset = CMTime.zero
    private var metricsTimer: DispatchSourceTimer?
    private var lastPTS: CMTime?
    private var didReportWriterFailure = false
    private let nativeWidth: Int
    private let nativeHeight: Int
    private let outputWidth: Int
    private let outputHeight: Int

    init(outputURL: URL, width: Int, height: Int, nativeWidth: Int, nativeHeight: Int, fps: Int32, codec: String, quality: String, includeSystemAudio: Bool, includeMicrophone: Bool) throws {
        self.nativeWidth = nativeWidth
        self.nativeHeight = nativeHeight
        self.outputWidth = width
        self.outputHeight = height
        writer = try AVAssetWriter(outputURL: outputURL, fileType: .mov)

        let bitrate = targetBitrate(width: width, height: height, fps: fps, quality: quality, codec: codec)
        let compression: [String: Any] = [
            AVVideoAverageBitRateKey: bitrate,
            AVVideoExpectedSourceFrameRateKey: Int(fps),
            AVVideoMaxKeyFrameIntervalKey: Int(fps) * 2,
            AVVideoAllowFrameReorderingKey: false,
        ]
        let videoSettings: [String: Any] = [
            AVVideoCodecKey: codec == "h264" ? AVVideoCodecType.h264 : AVVideoCodecType.hevc,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: compression,
        ]

        videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
        videoInput.expectsMediaDataInRealTime = true

        guard writer.canAdd(videoInput) else {
            throw HelperError.writerInputRejected
        }
        writer.add(videoInput)

        if includeSystemAudio {
            let audioSettings: [String: Any] = [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 128_000,
            ]
            let input = AVAssetWriterInput(mediaType: .audio, outputSettings: audioSettings)
            input.expectsMediaDataInRealTime = true

            guard writer.canAdd(input) else {
                throw HelperError.writerInputRejected
            }
            writer.add(input)
            audioInput = input
        } else {
            audioInput = nil
        }

        if includeMicrophone {
            // Mono keeps the converter safe for any input device channel count.
            let micSettings: [String: Any] = [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 1,
                AVEncoderBitRateKey: 96_000,
            ]
            let input = AVAssetWriterInput(mediaType: .audio, outputSettings: micSettings)
            input.expectsMediaDataInRealTime = true

            guard writer.canAdd(input) else {
                throw HelperError.writerInputRejected
            }
            writer.add(input)
            micInput = input
        } else {
            micInput = nil
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        emitFailure("stream stopped with error: \(error)")
    }

    // Writer errors are fatal for the file; signal once, then keep logging.
    private func reportWriterFailure(_ message: String) {
        if didReportWriterFailure {
            logLine(message)
            return
        }
        didReportWriterFailure = true
        emitFailure(message)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        switch outputType {
        case .screen:
            appendVideo(sampleBuffer)
        case .audio:
            appendAudio(sampleBuffer)
        case .microphone:
            appendMicrophone(sampleBuffer)
        default:
            return
        }
    }

    private func appendVideo(_ sampleBuffer: CMSampleBuffer) {
        guard sampleBuffer.isValid else {
            droppedFrameCount += 1
            return
        }
        guard frameStatus(sampleBuffer) == .complete else {
            droppedFrameCount += 1
            return
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        guard pts.isValid else {
            droppedFrameCount += 1
            return
        }
        if isPaused {
            if pauseStartedPTS == nil {
                pauseStartedPTS = pts
            }
            // Paused frames are intentionally discarded, not dropped.
            return
        }
        applyPendingResume(at: pts)
        guard let sampleBuffer = retimedSampleBuffer(sampleBuffer, subtracting: pauseOffset) else {
            droppedFrameCount += 1
            return
        }
        let adjustedPTS = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)

        if !didStart {
            // startWriting throws NSInternalInconsistencyException if called
            // again after a failure, so only ever attempt it from .unknown.
            guard writer.status == .unknown else {
                droppedFrameCount += 1
                return
            }
            guard writer.startWriting() else {
                reportWriterFailure("writer failed to start: \(writer.error?.localizedDescription ?? "unknown")")
                droppedFrameCount += 1
                return
            }
            writer.startSession(atSourceTime: adjustedPTS)
            firstPTS = adjustedPTS
            didStart = true
            logLine("recording started")
            emitEvent([
                "event": "started",
                "native_width": nativeWidth,
                "native_height": nativeHeight,
                "output_width": outputWidth,
                "output_height": outputHeight,
            ])
            startMetricsTimer()
        }

        guard videoInput.isReadyForMoreMediaData else {
            droppedFrameCount += 1
            return
        }

        if videoInput.append(sampleBuffer) {
            frameCount += 1
            lastPTS = adjustedPTS
        } else {
            droppedFrameCount += 1
            if let error = writer.error {
                reportWriterFailure("video append failed: \(error)")
            }
        }
    }

    private func appendAudio(_ sampleBuffer: CMSampleBuffer) {
        guard let audioInput, !isPaused, !pendingResume else {
            return
        }
        if appendAudioSample(sampleBuffer, to: audioInput, label: "audio") {
            audioSampleCount += 1
        } else {
            droppedAudioSampleCount += 1
        }
    }

    private func appendMicrophone(_ sampleBuffer: CMSampleBuffer) {
        guard let micInput, !isPaused, !pendingResume else {
            return
        }
        if appendAudioSample(sampleBuffer, to: micInput, label: "microphone") {
            micSampleCount += 1
        } else {
            droppedMicSampleCount += 1
        }
    }


    private func appendAudioSample(_ sampleBuffer: CMSampleBuffer, to input: AVAssetWriterInput, label: String) -> Bool {
        guard didStart, let firstPTS else {
            return false
        }
        guard sampleBuffer.isValid else {
            return false
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        guard pts.isValid, CMTimeCompare(pts, firstPTS) >= 0 else {
            return false
        }
        if isPaused || pendingResume {
            return false
        }
        guard let sampleBuffer = retimedSampleBuffer(sampleBuffer, subtracting: pauseOffset) else {
            return false
        }
        guard input.isReadyForMoreMediaData else {
            return false
        }

        if input.append(sampleBuffer) {
            return true
        }
        if let error = writer.error {
            reportWriterFailure("\(label) append failed: \(error)")
        }
        return false
    }

    func pause() {
        queue.async {
            guard self.didStart, !self.didFinish, !self.isPaused else {
                return
            }

            self.isPaused = true
            self.pendingResume = false
            self.pauseStartedPTS = nil
            logLine("recording paused")
        }
    }

    func resume() {
        queue.async {
            guard self.didStart, !self.didFinish, self.isPaused else {
                return
            }

            self.isPaused = false
            self.pendingResume = true
            logLine("recording resumed")
        }
    }

    private func applyPendingResume(at pts: CMTime) {
        guard pendingResume else {
            return
        }

        if let pauseStartedPTS, CMTimeCompare(pts, pauseStartedPTS) >= 0 {
            pauseOffset = CMTimeAdd(pauseOffset, CMTimeSubtract(pts, pauseStartedPTS))
        }
        pendingResume = false
        pauseStartedPTS = nil
    }

    private func retimedSampleBuffer(_ sampleBuffer: CMSampleBuffer, subtracting offset: CMTime) -> CMSampleBuffer? {
        guard offset.isValid, CMTimeCompare(offset, .zero) > 0 else {
            return sampleBuffer
        }

        var timingCount = 0
        var status = CMSampleBufferGetSampleTimingInfoArray(
            sampleBuffer,
            entryCount: 0,
            arrayToFill: nil,
            entriesNeededOut: &timingCount
        )
        guard status == noErr, timingCount > 0 else {
            return sampleBuffer
        }

        var timing = Array(repeating: CMSampleTimingInfo(), count: timingCount)
        status = timing.withUnsafeMutableBufferPointer { buffer in
            CMSampleBufferGetSampleTimingInfoArray(
                sampleBuffer,
                entryCount: timingCount,
                arrayToFill: buffer.baseAddress,
                entriesNeededOut: &timingCount
            )
        }
        guard status == noErr else {
            return nil
        }

        for index in timing.indices {
            if timing[index].presentationTimeStamp.isValid {
                timing[index].presentationTimeStamp = CMTimeSubtract(timing[index].presentationTimeStamp, offset)
            }
            if timing[index].decodeTimeStamp.isValid {
                timing[index].decodeTimeStamp = CMTimeSubtract(timing[index].decodeTimeStamp, offset)
            }
        }

        var adjusted: CMSampleBuffer?
        status = timing.withUnsafeBufferPointer { buffer in
            CMSampleBufferCreateCopyWithNewTiming(
                allocator: kCFAllocatorDefault,
                sampleBuffer: sampleBuffer,
                sampleTimingEntryCount: timingCount,
                sampleTimingArray: buffer.baseAddress,
                sampleBufferOut: &adjusted
            )
        }
        guard status == noErr else {
            return nil
        }
        return adjusted
    }

    func finish(timeout: DispatchTimeInterval) -> Bool {
        queue.async {
            guard !self.didFinish else {
                self.finished.signal()
                return
            }

            self.didFinish = true
            self.metricsTimer?.cancel()
            self.metricsTimer = nil
            if !self.didStart && self.writer.status == .unknown {
                self.writer.startWriting()
                self.writer.startSession(atSourceTime: .zero)
            }
            guard self.writer.status == .writing else {
                self.reportWriterFailure(
                    "writer cannot finalize: \(self.writer.error?.localizedDescription ?? "status \(self.writer.status.rawValue)")"
                )
                self.finished.signal()
                return
            }

            self.videoInput.markAsFinished()
            self.audioInput?.markAsFinished()
            self.micInput?.markAsFinished()
            self.writer.finishWriting {
                if let error = self.writer.error {
                    self.reportWriterFailure("writer finish failed: \(error)")
                } else {
                    logLine("recording finished frames=\(self.frameCount) dropped=\(self.droppedFrameCount) audio=\(self.audioSampleCount) audio_dropped=\(self.droppedAudioSampleCount) mic=\(self.micSampleCount) mic_dropped=\(self.droppedMicSampleCount)")
                }
                self.finished.signal()
            }
        }

        return finished.wait(timeout: .now() + timeout) == .success
    }

    // ScreenCaptureKit only delivers frames when screen content changes, so
    // metrics run on a timer rather than the frame path — the heartbeat must
    // keep flowing while a static screen is recorded.
    private func startMetricsTimer() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 1, repeating: 1)
        timer.setEventHandler { [weak self] in
            self?.emitMetrics()
        }
        timer.resume()
        metricsTimer = timer
    }

    private func emitMetrics() {
        guard didStart, !didFinish else {
            return
        }
        // elapsed is media time (last appended PTS minus first), not wall
        // clock: it matches the duration of the written file, freezes during
        // pause, and lags wall time while the screen is static.
        let elapsed = zip2(firstPTS, lastPTS).map { CMTimeSubtract($1, $0).seconds } ?? 0
        let elapsedSeconds = max(0, Int64(elapsed.rounded()))
        emitEvent([
            "event": "metrics",
            "elapsed": elapsedSeconds,
            "frames": frameCount,
            "dropped": droppedFrameCount,
        ])
    }

    // Reports whether the writer failed at any point; safe to call after
    // finish() has returned.
    func hadWriterFailure() -> Bool {
        queue.sync { didReportWriterFailure }
    }
}

func zip2<A, B>(_ a: A?, _ b: B?) -> (A, B)? {
    guard let a, let b else {
        return nil
    }
    return (a, b)
}

enum HelperError: Error {
    case writerInputRejected
}

@MainActor
func run() async {
    let args = CommandLine.arguments

    if args.count >= 2 && args[1] == "--permission-status" {
        print(CGPreflightScreenCaptureAccess() ? "granted" : "missing")
        return
    }

    if args.count >= 2 && args[1] == "--request-permission" {
        print(CGRequestScreenCaptureAccess() ? "granted" : "missing")
        return
    }

    if args.count >= 2 && args[1] == "--mic-permission-status" {
        print(microphonePermissionGranted() ? "granted" : "missing")
        return
    }

    if args.count >= 2 && args[1] == "--request-mic-permission" {
        let granted = await requestMicrophonePermission()
        print(granted ? "granted" : "missing")
        return
    }

    if args.count >= 2 && args[1] == "--list" {
        guard ensureScreenCapturePermission() else {
            fputs("capture-engine: permission denied: Screen Recording access is required\n", stderr)
            Foundation.exit(Exit.noPermission)
        }
        await listTargets()
        return
    }

    guard args.count >= 9 else {
        fputs("usage: capture-engine <output-path> <fps> <include-cursor> <display|window> <id> <hevc|h264> <efficient|balanced|high> <native|720p|1080p|2k|4k> [include-system-audio] [hide-wrec] [include-microphone] [show-mic-indicator]\n", stderr)
        Foundation.exit(Exit.usage)
    }

    let outputPath = args[1]
    guard let fps = Int32(args[2]), fps == 30 || fps == 60 else {
        fputs("capture-engine: invalid fps `\(args[2])` (expected 30 or 60)\n", stderr)
        Foundation.exit(Exit.usage)
    }
    let includeCursor = args[3] == "true"
    let targetKind = args[4]
    let targetId = UInt32(args[5]) ?? 0
    let codec = args[6]
    let quality = args[7]
    let resolution = args[8]
    let includeSystemAudio = args.count >= 10 ? args[9] == "true" : false
    let hideWrec = args.count >= 11 ? args[10] == "true" : true
    let includeMicrophone = args.count >= 12 ? args[11] == "true" : false
    // Defaults on so a caller that says nothing gets the indicator; the app
    // opts out because its user toggled the mic in visible UI.
    let showMicIndicator = args.count >= 13 ? args[12] == "true" : true

    guard ensureScreenCapturePermission() else {
        emitFailure("permission denied: Screen Recording access is required")
        Foundation.exit(Exit.noPermission)
    }

    if includeMicrophone && !microphonePermissionGranted() {
        emitFailure("permission denied: Microphone access is required. Grant it in System Settings > Privacy & Security > Microphone, or disable the microphone toggle.")
        Foundation.exit(Exit.noPermission)
    }

    let micIndicator = MicIndicator()
    let wantsMicIndicator = (includeMicrophone && showMicIndicator) || MicIndicator.testHookEnabled

    do {
        // The pill must exist before the shareable-content snapshot for
        // display capture so its window can be excluded from the recording.
        if wantsMicIndicator && targetKind != "window" {
            micIndicator.show(on: screenForDisplay(targetId))
        }

        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let filter: SCContentFilter
        let fallbackWidth: Int
        let fallbackHeight: Int

        if targetKind == "window" {
            guard let window = content.windows.first(where: { $0.windowID == targetId }) else {
                emitFailure("window not found")
                Foundation.exit(Exit.unavailable)
            }
            // Window capture sees only the target window, so the pill needs
            // no exclusion in this mode.
            if wantsMicIndicator {
                micIndicator.show(on: screenContaining(windowFrame: window.frame))
            }
            filter = SCContentFilter(desktopIndependentWindow: window)
            fallbackWidth = Int(window.frame.width)
            fallbackHeight = Int(window.frame.height)
        } else {
            let display = content.displays.first(where: { $0.displayID == targetId }) ?? content.displays.first
            guard let display else {
                emitFailure("no display found")
                Foundation.exit(Exit.unavailable)
            }
            // The engine's own windows (the mic pill) are never meaningful
            // recording content, so they are excluded even when hideWrec is
            // off.
            var excludedWindows = ownWindows(in: content)
            if hideWrec {
                let wrec = wrecWindows(in: content)
                logLine("excluding \(wrec.count) Wrec window(s)")
                excludedWindows.append(contentsOf: wrec)
            }
            filter = SCContentFilter(display: display, excludingWindows: excludedWindows)
            fallbackWidth = display.width
            fallbackHeight = display.height
        }

        let nativeSize = nativeCaptureSize(
            filter: filter,
            fallbackWidth: fallbackWidth,
            fallbackHeight: fallbackHeight
        )
        let captureSize = outputSize(
            nativeWidth: nativeSize.width,
            nativeHeight: nativeSize.height,
            resolution: resolution
        )
        let captureWidth = captureSize.width
        let captureHeight = captureSize.height

        let streamConfig = SCStreamConfiguration()
        streamConfig.width = captureWidth
        streamConfig.height = captureHeight
        streamConfig.scalesToFit = true
        streamConfig.minimumFrameInterval = CMTime(value: 1, timescale: fps)
        streamConfig.queueDepth = quality == "high" ? 4 : 2
        streamConfig.showsCursor = includeCursor
        streamConfig.capturesAudio = includeSystemAudio
        streamConfig.excludesCurrentProcessAudio = true
        streamConfig.sampleRate = 48_000
        streamConfig.channelCount = 2
        // nil device ID leaves ScreenCaptureKit on the system default microphone.
        streamConfig.captureMicrophone = includeMicrophone
        streamConfig.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        logLine("target=\(targetKind) id=\(targetId) native=\(nativeSize.width)x\(nativeSize.height) size=\(captureWidth)x\(captureHeight) fps=\(fps) cursor=\(includeCursor) system_audio=\(includeSystemAudio) microphone=\(includeMicrophone) codec=\(codec) quality=\(quality) resolution=\(resolution) pipeline=scstream-avassetwriter")

        let outputURL = URL(fileURLWithPath: outputPath)
        let recorder = try SampleRecorder(
            outputURL: outputURL,
            width: captureWidth,
            height: captureHeight,
            nativeWidth: nativeSize.width,
            nativeHeight: nativeSize.height,
            fps: fps,
            codec: codec,
            quality: quality,
            includeSystemAudio: includeSystemAudio,
            includeMicrophone: includeMicrophone
        )
        let stream = SCStream(filter: filter, configuration: streamConfig, delegate: recorder)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: recorder.queue)
        if includeSystemAudio {
            try stream.addStreamOutput(recorder, type: .audio, sampleHandlerQueue: recorder.queue)
        }
        if includeMicrophone {
            try stream.addStreamOutput(recorder, type: .microphone, sampleHandlerQueue: recorder.queue)
        }

        try await stream.startCapture()

        // Parent process writes commands to stdin. EOF also stops.
        await waitForStopSignal(recorder: recorder)

        try await stream.stopCapture()
        micIndicator.hide()
        guard recorder.finish(timeout: .seconds(15)) else {
            emitFailure("timed out waiting for writer finalization")
            Foundation.exit(Exit.ioError)
        }
        if recorder.hadWriterFailure() {
            Foundation.exit(Exit.ioError)
        }
    } catch {
        emitFailure("error: \(error)")
        Foundation.exit(Exit.software)
    }
}

func frameStatus(_ sampleBuffer: CMSampleBuffer) -> SCFrameStatus {
    guard
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
        let rawStatus = attachments.first?[SCStreamFrameInfo.status] as? Int,
        let status = SCFrameStatus(rawValue: rawStatus)
    else {
        return .complete
    }
    return status
}

func evenDimension(_ value: Int) -> Int {
    max(2, value - (value % 2))
}

func nativeCaptureSize(filter: SCContentFilter, fallbackWidth: Int, fallbackHeight: Int) -> (width: Int, height: Int) {
    let scale = CGFloat(filter.pointPixelScale)
    let width = evenDimension(Int((filter.contentRect.width * scale).rounded()))
    let height = evenDimension(Int((filter.contentRect.height * scale).rounded()))

    if width > 2 && height > 2 {
        return (width, height)
    }
    return (evenDimension(fallbackWidth), evenDimension(fallbackHeight))
}

func outputSize(nativeWidth: Int, nativeHeight: Int, resolution: String) -> (width: Int, height: Int) {
    let maxSize: (width: Int, height: Int)? = switch resolution {
    case "720p": (1280, 720)
    case "1080p": (1920, 1080)
    case "2k": (2560, 1440)
    case "4k": (3840, 2160)
    default: nil
    }

    guard let maxSize else {
        return (evenDimension(nativeWidth), evenDimension(nativeHeight))
    }

    let scale = min(
        1.0,
        Double(maxSize.width) / Double(nativeWidth),
        Double(maxSize.height) / Double(nativeHeight)
    )
    return (
        evenDimension(Int((Double(nativeWidth) * scale).rounded())),
        evenDimension(Int((Double(nativeHeight) * scale).rounded()))
    )
}

func ensureScreenCapturePermission() -> Bool {
    if CGPreflightScreenCaptureAccess() {
        return true
    }
    return CGRequestScreenCaptureAccess()
}

func microphonePermissionGranted() -> Bool {
    AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
}

func requestMicrophonePermission() async -> Bool {
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized:
        return true
    case .notDetermined:
        return await AVCaptureDevice.requestAccess(for: .audio)
    default:
        return false
    }
}

func wrecWindows(in content: SCShareableContent) -> [SCWindow] {
    let parentPID = getppid()
    return content.windows.filter { isWrecWindow($0, parentPID: parentPID) }
}

// Windows owned by this capture-engine process itself (the mic pill).
func ownWindows(in content: SCShareableContent) -> [SCWindow] {
    let pid = getpid()
    return content.windows.filter { $0.owningApplication?.processID == pid }
}

// The engine's parent is the daemon, which owns no windows, so the ppid check
// alone never matches the GPUI app. Match wrec by identity as well.
func isWrecWindow(_ window: SCWindow, parentPID: pid_t) -> Bool {
    guard let app = window.owningApplication else {
        return false
    }
    if app.processID == parentPID {
        return true
    }
    if app.bundleIdentifier.hasPrefix("app.wrec") {
        return true
    }
    let name = app.applicationName.lowercased()
    return name == "wrec" || name == "wrec dev" || name == "wrec-app"
}

@MainActor
func initializeGraphicsClient() {
    _ = NSApplication.shared
    NSApplication.shared.setActivationPolicy(.prohibited)
}

func waitForStopSignal(recorder: SampleRecorder) async {
    let stopped = DispatchSemaphore(value: 0)
    DispatchQueue.global(qos: .userInitiated).async {
        while let line = readLine() {
            switch line.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
            case "pause":
                recorder.pause()
            case "resume":
                recorder.resume()
            case "stop":
                stopped.signal()
                return
            default:
                continue
            }
        }
        stopped.signal()
    }

    await Task.detached(priority: .userInitiated) {
        stopped.wait()
    }.value
}

func targetBitrate(width: Int, height: Int, fps: Int32, quality: String, codec: String) -> Int {
    let pixelsPerSecond = Double(width * height * Int(fps))
    let bitsPerPixel = switch quality {
    case "efficient": 0.045
    case "high": 0.105
    default: 0.07
    }
    let codecScale = codec == "h264" ? 1.35 : 1.0
    return max(1_500_000, Int(pixelsPerSecond * bitsPerPixel * codecScale))
}

@MainActor
func listTargets() async {
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        for display in content.displays {
            print("display\t\(display.displayID)\tDisplay \(display.displayID)")
        }
        for window in content.windows {
            if isWrecWindow(window, parentPID: getppid()) {
                continue
            }
            let appName = window.owningApplication?.applicationName ?? "App"
            let title = window.title ?? "Window"
            let name = "\(appName) — \(title)".replacingOccurrences(of: "\t", with: " ")
            if window.frame.width >= 64 && window.frame.height >= 64 {
                print("window\t\(window.windowID)\t\(name)")
            }
        }
    } catch {
        fputs("capture-engine: list error: \(error)\n", stderr)
        Foundation.exit(Exit.software)
    }
}

@main
struct WrecHelper {
    static func main() async {
        await initializeGraphicsClient()
        await run()
    }
}
