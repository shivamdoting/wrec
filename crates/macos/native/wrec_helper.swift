import Foundation
import AppKit
import ScreenCaptureKit
import AVFoundation
import AudioToolbox
import CoreGraphics
import Darwin
import CoreMedia
import CoreVideo

final class SampleRecorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let queue = DispatchQueue(label: "wrec.capture.writer", qos: .userInitiated)

    private let writer: AVAssetWriter
    private let videoInput: AVAssetWriterInput
    private let audioInput: AVAssetWriterInput?
    private let started = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)
    private var didStart = false
    private var didFinish = false
    private var frameCount: Int64 = 0
    private var droppedFrameCount: Int64 = 0
    private var audioSampleCount: Int64 = 0
    private var droppedAudioSampleCount: Int64 = 0
    private var firstPTS: CMTime?
    private var isPaused = false
    private var pauseStartedPTS: CMTime?
    private var pendingResume = false
    private var pauseOffset = CMTime.zero
    private var lastMetricTime = DispatchTime.now()

    init(outputURL: URL, width: Int, height: Int, fps: Int32, codec: String, quality: String, includeSystemAudio: Bool) throws {
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
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write(Data("wrec-helper: stream stopped with error: \(error)\n".utf8))
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        switch outputType {
        case .screen:
            appendVideo(sampleBuffer)
        case .audio:
            appendAudio(sampleBuffer)
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
            droppedFrameCount += 1
            return
        }
        applyPendingResume(at: pts)
        guard let sampleBuffer = retimedSampleBuffer(sampleBuffer, subtracting: pauseOffset) else {
            droppedFrameCount += 1
            return
        }
        let adjustedPTS = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)

        if !didStart {
            guard writer.startWriting() else {
                FileHandle.standardError.write(Data("wrec-helper: writer failed to start: \(writer.error?.localizedDescription ?? "unknown")\n".utf8))
                droppedFrameCount += 1
                return
            }
            writer.startSession(atSourceTime: adjustedPTS)
            firstPTS = adjustedPTS
            didStart = true
            FileHandle.standardError.write(Data("wrec-helper: recording started\n".utf8))
            started.signal()
        }

        guard videoInput.isReadyForMoreMediaData else {
            droppedFrameCount += 1
            return
        }

        if videoInput.append(sampleBuffer) {
            frameCount += 1
            emitMetricsIfNeeded(currentPTS: adjustedPTS)
        } else {
            droppedFrameCount += 1
            if let error = writer.error {
                FileHandle.standardError.write(Data("wrec-helper: video append failed: \(error)\n".utf8))
            }
        }
    }

    private func appendAudio(_ sampleBuffer: CMSampleBuffer) {
        guard let audioInput else {
            return
        }
        guard didStart, let firstPTS else {
            droppedAudioSampleCount += 1
            return
        }
        guard sampleBuffer.isValid else {
            droppedAudioSampleCount += 1
            return
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        guard pts.isValid, CMTimeCompare(pts, firstPTS) >= 0 else {
            droppedAudioSampleCount += 1
            return
        }
        if isPaused || pendingResume {
            droppedAudioSampleCount += 1
            return
        }
        guard let sampleBuffer = retimedSampleBuffer(sampleBuffer, subtracting: pauseOffset) else {
            droppedAudioSampleCount += 1
            return
        }
        guard audioInput.isReadyForMoreMediaData else {
            droppedAudioSampleCount += 1
            return
        }

        if audioInput.append(sampleBuffer) {
            audioSampleCount += 1
        } else {
            droppedAudioSampleCount += 1
            if let error = writer.error {
                FileHandle.standardError.write(Data("wrec-helper: audio append failed: \(error)\n".utf8))
            }
        }
    }

    func pause() {
        queue.async {
            guard self.didStart, !self.didFinish, !self.isPaused else {
                return
            }

            self.isPaused = true
            self.pendingResume = false
            self.pauseStartedPTS = nil
            FileHandle.standardError.write(Data("wrec-helper: recording paused\n".utf8))
        }
    }

    func resume() {
        queue.async {
            guard self.didStart, !self.didFinish, self.isPaused else {
                return
            }

            self.isPaused = false
            self.pendingResume = true
            FileHandle.standardError.write(Data("wrec-helper: recording resumed\n".utf8))
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

    func waitUntilStarted(timeout: DispatchTimeInterval) -> Bool {
        started.wait(timeout: .now() + timeout) == .success
    }

    func finish(timeout: DispatchTimeInterval) -> Bool {
        queue.async {
            guard !self.didFinish else {
                self.finished.signal()
                return
            }

            self.didFinish = true
            if !self.didStart {
                self.writer.startWriting()
                self.writer.startSession(atSourceTime: .zero)
            }

            self.videoInput.markAsFinished()
            self.audioInput?.markAsFinished()
            self.writer.finishWriting {
                if let error = self.writer.error {
                    FileHandle.standardError.write(Data("wrec-helper: writer finish failed: \(error)\n".utf8))
                } else {
                    FileHandle.standardError.write(Data("wrec-helper: recording finished frames=\(self.frameCount) dropped=\(self.droppedFrameCount) audio=\(self.audioSampleCount) audio_dropped=\(self.droppedAudioSampleCount)\n".utf8))
                }
                self.finished.signal()
            }
        }

        return finished.wait(timeout: .now() + timeout) == .success
    }

    private func emitMetricsIfNeeded(currentPTS: CMTime) {
        let now = DispatchTime.now()
        guard now.uptimeNanoseconds - lastMetricTime.uptimeNanoseconds >= 1_000_000_000 else {
            return
        }
        lastMetricTime = now

        let elapsed = firstPTS.map { CMTimeSubtract(currentPTS, $0).seconds } ?? 0
        let elapsedSeconds = max(0, Int64(elapsed.rounded()))
        FileHandle.standardError.write(
            Data("wrec-helper: metrics elapsed=\(elapsedSeconds) frames=\(frameCount) dropped=\(droppedFrameCount)\n".utf8)
        )
    }
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

    if args.count >= 2 && args[1] == "--list" {
        guard ensureScreenCapturePermission() else {
            fputs("wrec-helper: permission denied: Screen Recording access is required\n", stderr)
            Foundation.exit(13)
        }
        await listTargets()
        return
    }

    guard args.count >= 9 else {
        fputs("usage: wrec_helper <output-path> <fps> <include-cursor> <display|window> <id> <hevc|h264> <efficient|balanced|high> <native|720p|1080p|2k|4k> [include-system-audio] [hide-wrec]\n", stderr)
        Foundation.exit(64)
    }

    let outputPath = args[1]
    let fps = Int32(args[2]) ?? 60
    let includeCursor = args[3] == "true"
    let targetKind = args[4]
    let targetId = UInt32(args[5]) ?? 0
    let codec = args[6]
    let quality = args[7]
    let resolution = args[8]
    let includeSystemAudio = args.count >= 10 ? args[9] == "true" : false
    let hideWrec = args.count >= 11 ? args[10] == "true" : true

    guard ensureScreenCapturePermission() else {
        fputs("wrec-helper: permission denied: Screen Recording access is required\n", stderr)
        Foundation.exit(13)
    }

    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let filter: SCContentFilter
        let fallbackWidth: Int
        let fallbackHeight: Int

        if targetKind == "window" {
            guard let window = content.windows.first(where: { $0.windowID == targetId }) else {
                fputs("wrec-helper: window not found\n", stderr)
                Foundation.exit(5)
            }
            filter = SCContentFilter(desktopIndependentWindow: window)
            fallbackWidth = Int(window.frame.width)
            fallbackHeight = Int(window.frame.height)
        } else {
            let display = content.displays.first(where: { $0.displayID == targetId }) ?? content.displays.first
            guard let display else {
                fputs("wrec-helper: no display found\n", stderr)
                Foundation.exit(4)
            }
            let excludedWindows = hideWrec ? wrecWindows(in: content) : []
            if hideWrec {
                FileHandle.standardError.write(
                    Data("wrec-helper: excluding \(excludedWindows.count) Wrec window(s)\n".utf8)
                )
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
        streamConfig.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        FileHandle.standardError.write(
            Data(
                "wrec-helper: target=\(targetKind) id=\(targetId) native=\(nativeSize.width)x\(nativeSize.height) size=\(captureWidth)x\(captureHeight) fps=\(fps) cursor=\(includeCursor) system_audio=\(includeSystemAudio) codec=\(codec) quality=\(quality) resolution=\(resolution) pipeline=scstream-avassetwriter\n"
                    .utf8
            )
        )

        let outputURL = URL(fileURLWithPath: outputPath)
        let recorder = try SampleRecorder(
            outputURL: outputURL,
            width: captureWidth,
            height: captureHeight,
            fps: fps,
            codec: codec,
            quality: quality,
            includeSystemAudio: includeSystemAudio
        )
        let stream = SCStream(filter: filter, configuration: streamConfig, delegate: recorder)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: recorder.queue)
        if includeSystemAudio {
            try stream.addStreamOutput(recorder, type: .audio, sampleHandlerQueue: recorder.queue)
        }

        try await stream.startCapture()
        _ = recorder.waitUntilStarted(timeout: .seconds(3))

        // Parent process writes commands to stdin. EOF also stops.
        await waitForStopSignal(recorder: recorder)

        try await stream.stopCapture()
        guard recorder.finish(timeout: .seconds(15)) else {
            fputs("wrec-helper: timed out waiting for writer finalization\n", stderr)
            Foundation.exit(6)
        }
    } catch {
        fputs("wrec-helper: error: \(error)\n", stderr)
        Foundation.exit(1)
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

func wrecWindows(in content: SCShareableContent) -> [SCWindow] {
    let wrecProcessID = getppid()
    return content.windows.filter { window in
        window.owningApplication?.processID == wrecProcessID
    }
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
            if window.owningApplication?.processID == getppid() {
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
        fputs("wrec-helper: list error: \(error)\n", stderr)
        Foundation.exit(1)
    }
}

@main
struct WrecHelper {
    static func main() async {
        await initializeGraphicsClient()
        await run()
    }
}
