import Foundation
import AppKit
import ScreenCaptureKit
import AVFoundation
import AudioToolbox
import CoreGraphics
import Darwin
import CoreMedia
import CoreVideo
import ImageIO
import UniformTypeIdentifiers
import VideoToolbox

protocol CaptureRecorder: NSObjectProtocol, SCStreamOutput, SCStreamDelegate {
    var queue: DispatchQueue { get }

    func pause()
    func resume()
    func finish(timeout: DispatchTimeInterval) -> Bool
}

final class SampleRecorder: NSObject, CaptureRecorder {
    let queue = DispatchQueue(label: "wrec.capture.writer", qos: .userInitiated)

    private let writer: AVAssetWriter
    private let videoInput: AVAssetWriterInput
    private let audioInput: AVAssetWriterInput?
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

private struct FrameFingerprint {
    let samples: [UInt8]

    func distance(to other: FrameFingerprint) -> Int {
        zip(samples, other.samples).reduce(0) { total, pair in
            total + abs(Int(pair.0) - Int(pair.1))
        }
    }
}

final class GifRecorder: NSObject, CaptureRecorder {
    let queue = DispatchQueue(label: "wrec.capture.gif-writer", qos: .userInitiated)

    private let destination: CGImageDestination
    private let frameDelay: Double
    private let finished = DispatchSemaphore(value: 0)
    private var didStart = false
    private var didFinish = false
    private var finalizeSucceeded = false
    private var frameCount: Int64 = 0
    private var droppedFrameCount: Int64 = 0
    private var skippedFrameCount: Int64 = 0
    private var firstPTS: CMTime?
    private var pendingImage: CGImage?
    private var pendingPTS: CMTime?
    private var latestPTS: CMTime?
    private var lastFingerprint: FrameFingerprint?
    private var isPaused = false
    private var pauseStartedPTS: CMTime?
    private var pendingResume = false
    private var pauseOffset = CMTime.zero
    private var lastMetricTime = DispatchTime.now()

    init(outputURL: URL, fps: Int32) throws {
        guard let destination = CGImageDestinationCreateWithURL(outputURL as CFURL, UTType.gif.identifier as CFString, 0, nil) else {
            throw HelperError.writerInputRejected
        }

        self.destination = destination
        frameDelay = max(1.0 / Double(max(fps, 1)), 0.02)
        let gifProperties = [
            kCGImagePropertyGIFDictionary as String: [
                kCGImagePropertyGIFLoopCount as String: 0,
            ],
        ]
        CGImageDestinationSetProperties(destination, gifProperties as CFDictionary)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write(Data("wrec-helper: stream stopped with error: \(error)\n".utf8))
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        guard outputType == .screen else {
            return
        }
        appendFrame(sampleBuffer)
    }

    private func appendFrame(_ sampleBuffer: CMSampleBuffer) {
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
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else {
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
        let adjustedPTS = CMTimeSubtract(pts, pauseOffset)
        latestPTS = adjustedPTS

        guard let fingerprint = frameFingerprint(pixelBuffer) else {
            droppedFrameCount += 1
            return
        }
        if let lastFingerprint, fingerprint.distance(to: lastFingerprint) <= 96 {
            skippedFrameCount += 1
            emitMetricsIfNeeded(currentPTS: adjustedPTS)
            return
        }

        guard let cgImage = createCGImage(from: pixelBuffer) else {
            droppedFrameCount += 1
            FileHandle.standardError.write(Data("wrec-helper: gif frame conversion failed\n".utf8))
            return
        }

        if !didStart {
            firstPTS = adjustedPTS
            didStart = true
            FileHandle.standardError.write(Data("wrec-helper: recording started\n".utf8))
        }

        if let pendingImage, let pendingPTS {
            addFrame(pendingImage, delay: frameDelay(from: pendingPTS, to: adjustedPTS))
        }
        pendingImage = cgImage
        pendingPTS = adjustedPTS
        lastFingerprint = fingerprint
        frameCount += 1
        emitMetricsIfNeeded(currentPTS: adjustedPTS)
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

    func finish(timeout: DispatchTimeInterval) -> Bool {
        queue.async {
            guard !self.didFinish else {
                self.finished.signal()
                return
            }

            self.didFinish = true
            if let pendingImage = self.pendingImage {
                let delay = self.pendingPTS
                    .flatMap { pendingPTS in
                        self.latestPTS.map { self.frameDelay(from: pendingPTS, to: $0) }
                    } ?? self.frameDelay
                self.addFrame(pendingImage, delay: delay)
                self.pendingImage = nil
                self.pendingPTS = nil
            }

            self.finalizeSucceeded = CGImageDestinationFinalize(self.destination)
            if self.finalizeSucceeded {
                FileHandle.standardError.write(Data("wrec-helper: recording finished frames=\(self.frameCount) dropped=\(self.droppedFrameCount) skipped=\(self.skippedFrameCount) audio=0 audio_dropped=0\n".utf8))
            } else {
                FileHandle.standardError.write(Data("wrec-helper: recording failed: gif finalization failed\n".utf8))
            }
            self.finished.signal()
        }

        return finished.wait(timeout: .now() + timeout) == .success && finalizeSucceeded
    }

    private func addFrame(_ image: CGImage, delay: Double) {
        let properties = [
            kCGImagePropertyGIFDictionary as String: [
                kCGImagePropertyGIFDelayTime as String: delay,
            ],
        ]
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
    }

    private func createCGImage(from pixelBuffer: CVPixelBuffer) -> CGImage? {
        var image: CGImage?
        let status = VTCreateCGImageFromCVPixelBuffer(pixelBuffer, options: nil, imageOut: &image)
        guard status == noErr else {
            return nil
        }
        return image
    }

    private func frameFingerprint(_ pixelBuffer: CVPixelBuffer) -> FrameFingerprint? {
        guard CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly) == kCVReturnSuccess else {
            return nil
        }
        defer {
            CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly)
        }

        guard let baseAddress = CVPixelBufferGetBaseAddress(pixelBuffer) else {
            return nil
        }

        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        let bytesPerRow = CVPixelBufferGetBytesPerRow(pixelBuffer)
        guard width > 0, height > 0, bytesPerRow >= width * 4 else {
            return nil
        }

        let gridWidth = 8
        let gridHeight = 8
        let bytes = baseAddress.assumingMemoryBound(to: UInt8.self)
        var samples = [UInt8]()
        samples.reserveCapacity(gridWidth * gridHeight)

        for gridY in 0..<gridHeight {
            let y = min(height - 1, (gridY * height + height / 2) / gridHeight)
            for gridX in 0..<gridWidth {
                let x = min(width - 1, (gridX * width + width / 2) / gridWidth)
                let offset = y * bytesPerRow + x * 4
                let blue = Int(bytes[offset])
                let green = Int(bytes[offset + 1])
                let red = Int(bytes[offset + 2])
                samples.append(UInt8((red * 54 + green * 183 + blue * 19) >> 8))
            }
        }

        return FrameFingerprint(samples: samples)
    }

    private func frameDelay(from previousPTS: CMTime, to currentPTS: CMTime) -> Double {
        let elapsed = CMTimeSubtract(currentPTS, previousPTS).seconds
        guard elapsed.isFinite, elapsed > 0 else {
            return frameDelay
        }
        return max(elapsed, 0.02)
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
            Data("wrec-helper: metrics elapsed=\(elapsedSeconds) frames=\(frameCount) dropped=\(droppedFrameCount) skipped=\(skippedFrameCount)\n".utf8)
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
        fputs("usage: wrec_helper <output-path> <fps> <include-cursor> <display|window> <id> <hevc|h264> <efficient|balanced|high> <native|720p|1080p|2k|4k> [include-system-audio] [hide-wrec] [mov|gif]\n", stderr)
        Foundation.exit(64)
    }

    let outputPath = args[1]
    let requestedFPS = Int32(args[2]) ?? 60
    let includeCursor = args[3] == "true"
    let targetKind = args[4]
    let targetId = UInt32(args[5]) ?? 0
    let codec = args[6]
    let quality = args[7]
    let requestedResolution = args[8]
    let includeSystemAudio = args.count >= 10 ? args[9] == "true" : false
    let hideWrec = args.count >= 11 ? args[10] == "true" : true
    let outputFormat = args.count >= 12 ? args[11] : "mov"
    let fps = outputFormat == "gif" ? min(max(requestedFPS, 1), 12) : requestedFPS
    let resolution = outputFormat == "gif" ? "720p" : requestedResolution
    let recordsSystemAudio = includeSystemAudio && outputFormat == "mov"

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
        streamConfig.queueDepth = outputFormat == "gif" ? 1 : (quality == "high" ? 4 : 2)
        streamConfig.showsCursor = includeCursor
        streamConfig.capturesAudio = recordsSystemAudio
        streamConfig.excludesCurrentProcessAudio = true
        streamConfig.sampleRate = 48_000
        streamConfig.channelCount = 2
        streamConfig.pixelFormat = outputFormat == "gif" ? kCVPixelFormatType_32BGRA : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        FileHandle.standardError.write(
            Data(
                "wrec-helper: target=\(targetKind) id=\(targetId) native=\(nativeSize.width)x\(nativeSize.height) size=\(captureWidth)x\(captureHeight) fps=\(fps) cursor=\(includeCursor) system_audio=\(recordsSystemAudio) format=\(outputFormat) codec=\(codec) quality=\(quality) resolution=\(resolution) pipeline=scstream-native-writer\n"
                    .utf8
            )
        )

        let outputURL = URL(fileURLWithPath: outputPath)
        let recorder: any CaptureRecorder
        if outputFormat == "gif" {
            recorder = try GifRecorder(outputURL: outputURL, fps: fps)
        } else {
            recorder = try SampleRecorder(
                outputURL: outputURL,
                width: captureWidth,
                height: captureHeight,
                fps: fps,
                codec: codec,
                quality: quality,
                includeSystemAudio: recordsSystemAudio
            )
        }
        try await recordCapture(
            filter: filter,
            streamConfig: streamConfig,
            recorder: recorder,
            includeSystemAudio: recordsSystemAudio
        )
    } catch {
        fputs("wrec-helper: error: \(error)\n", stderr)
        Foundation.exit(1)
    }
}

func recordCapture(
    filter: SCContentFilter,
    streamConfig: SCStreamConfiguration,
    recorder: any CaptureRecorder,
    includeSystemAudio: Bool
) async throws {
    let stream = SCStream(filter: filter, configuration: streamConfig, delegate: recorder)
    try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: recorder.queue)
    if includeSystemAudio {
        try stream.addStreamOutput(recorder, type: .audio, sampleHandlerQueue: recorder.queue)
    }

    try await stream.startCapture()

    // Parent process writes commands to stdin. EOF also stops.
    await waitForStopSignal(recorder: recorder)

    try await stream.stopCapture()
    guard recorder.finish(timeout: .seconds(15)) else {
        fputs("wrec-helper: timed out waiting for writer finalization\n", stderr)
        Foundation.exit(6)
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

func waitForStopSignal(recorder: any CaptureRecorder) async {
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
