import Foundation
import AppKit
import ScreenCaptureKit
import AVFoundation
import CoreGraphics
import CoreMedia
import CoreVideo

final class SampleRecorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let queue = DispatchQueue(label: "wrec.capture.writer", qos: .userInitiated)

    private let writer: AVAssetWriter
    private let input: AVAssetWriterInput
    private let started = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)
    private var didStart = false
    private var didFinish = false
    private var frameCount: Int64 = 0
    private var droppedFrameCount: Int64 = 0
    private var firstPTS: CMTime?
    private var lastMetricTime = DispatchTime.now()

    init(outputURL: URL, width: Int, height: Int, fps: Int32, codec: String, quality: String) throws {
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

        input = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
        input.expectsMediaDataInRealTime = true

        guard writer.canAdd(input) else {
            throw HelperError.writerInputRejected
        }
        writer.add(input)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write(Data("wrec-helper: stream stopped with error: \(error)\n".utf8))
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        guard outputType == .screen else {
            return
        }
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

        if !didStart {
            guard writer.startWriting() else {
                FileHandle.standardError.write(Data("wrec-helper: writer failed to start: \(writer.error?.localizedDescription ?? "unknown")\n".utf8))
                droppedFrameCount += 1
                return
            }
            writer.startSession(atSourceTime: pts)
            firstPTS = pts
            didStart = true
            FileHandle.standardError.write(Data("wrec-helper: recording started\n".utf8))
            started.signal()
        }

        guard input.isReadyForMoreMediaData else {
            droppedFrameCount += 1
            return
        }

        if input.append(sampleBuffer) {
            frameCount += 1
            emitMetricsIfNeeded(currentPTS: pts)
        } else {
            droppedFrameCount += 1
            if let error = writer.error {
                FileHandle.standardError.write(Data("wrec-helper: writer append failed: \(error)\n".utf8))
            }
        }
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

            self.input.markAsFinished()
            self.writer.finishWriting {
                if let error = self.writer.error {
                    FileHandle.standardError.write(Data("wrec-helper: writer finish failed: \(error)\n".utf8))
                } else {
                    FileHandle.standardError.write(Data("wrec-helper: recording finished frames=\(self.frameCount) dropped=\(self.droppedFrameCount)\n".utf8))
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
        fputs("usage: wrec_helper <output-path> <fps> <include-cursor> <display|window> <id> <hevc|h264> <efficient|balanced|high> <native|720p|1080p|2k|4k>\n", stderr)
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
            filter = SCContentFilter(display: display, excludingWindows: [])
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
        streamConfig.capturesAudio = false
        streamConfig.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        FileHandle.standardError.write(
            Data(
                "wrec-helper: target=\(targetKind) id=\(targetId) native=\(nativeSize.width)x\(nativeSize.height) size=\(captureWidth)x\(captureHeight) fps=\(fps) cursor=\(includeCursor) codec=\(codec) quality=\(quality) resolution=\(resolution) pipeline=scstream-avassetwriter\n"
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
            quality: quality
        )
        let stream = SCStream(filter: filter, configuration: streamConfig, delegate: recorder)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: recorder.queue)

        try await stream.startCapture()
        _ = recorder.waitUntilStarted(timeout: .seconds(3))

        // Parent process writes a line to stdin to stop. EOF also stops.
        await waitForStopSignal()

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

@MainActor
func initializeGraphicsClient() {
    _ = NSApplication.shared
    NSApplication.shared.setActivationPolicy(.prohibited)
}

func waitForStopSignal() async {
    await Task.detached(priority: .userInitiated) {
        _ = readLine()
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
