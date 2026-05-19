import Foundation
import ScreenCaptureKit
import AVFoundation
import CoreGraphics
import CoreMedia
import CoreVideo

final class RecordingDelegate: NSObject, SCRecordingOutputDelegate {
    func recordingOutputDidStartRecording(_ recordingOutput: SCRecordingOutput) {
        FileHandle.standardError.write(Data("wrec-helper: recording started\n".utf8))
    }

    func recordingOutput(_ recordingOutput: SCRecordingOutput, didFailWithError error: Error) {
        FileHandle.standardError.write(Data("wrec-helper: recording failed: \(error)\n".utf8))
        Foundation.exit(2)
    }

    func recordingOutputDidFinishRecording(_ recordingOutput: SCRecordingOutput) {
        FileHandle.standardError.write(Data("wrec-helper: recording finished\n".utf8))
    }
}

func run() async {
    guard #available(macOS 15.0, *) else {
        fputs("wrec-helper: SCRecordingOutput requires macOS 15+\n", stderr)
        Foundation.exit(3)
    }

    let args = CommandLine.arguments

    if args.count >= 2 && args[1] == "--list" {
        if !CGPreflightScreenCaptureAccess() {
            _ = CGRequestScreenCaptureAccess()
        }
        await listTargets()
        return
    }

    guard args.count >= 6 else {
        fputs("usage: wrec_helper.swift <output-path> <fps> <include-cursor> <display|window> <id>\n", stderr)
        Foundation.exit(64)
    }

    let outputPath = args[1]
    let fps = Int32(args[2]) ?? 60
    let includeCursor = args[3] == "true"
    let targetKind = args[4]
    let targetId = UInt32(args[5]) ?? 0

    if !CGPreflightScreenCaptureAccess() {
        _ = CGRequestScreenCaptureAccess()
    }

    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let filter: SCContentFilter
        let width: Int
        let height: Int

        if targetKind == "window" {
            guard let window = content.windows.first(where: { $0.windowID == targetId }) else {
                fputs("wrec-helper: window not found\n", stderr)
                Foundation.exit(5)
            }
            filter = SCContentFilter(desktopIndependentWindow: window)
            width = Int(window.frame.width)
            height = Int(window.frame.height)
        } else {
            let display = content.displays.first(where: { $0.displayID == targetId }) ?? content.displays.first
            guard let display else {
                fputs("wrec-helper: no display found\n", stderr)
                Foundation.exit(4)
            }
            filter = SCContentFilter(display: display, excludingWindows: [])
            width = display.width
            height = display.height
        }

        let streamConfig = SCStreamConfiguration()
        streamConfig.width = width - (width % 2)
        streamConfig.height = height - (height % 2)
        streamConfig.minimumFrameInterval = CMTime(value: 1, timescale: fps)
        streamConfig.queueDepth = 3
        streamConfig.showsCursor = includeCursor
        streamConfig.capturesAudio = false
        streamConfig.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange

        let stream = SCStream(filter: filter, configuration: streamConfig, delegate: nil)

        let recordingConfig = SCRecordingOutputConfiguration()
        recordingConfig.outputURL = URL(fileURLWithPath: outputPath)
        recordingConfig.outputFileType = .mov
        recordingConfig.videoCodecType = .hevc

        let delegate = RecordingDelegate()
        let recordingOutput = SCRecordingOutput(configuration: recordingConfig, delegate: delegate)
        try stream.addRecordingOutput(recordingOutput)

        try await stream.startCapture()

        // Parent process writes a line to stdin to stop. EOF also stops.
        _ = readLine()

        try await stream.stopCapture()
        // Give the recording delegate/writer a short moment to finalize the file.
        try? await Task.sleep(nanoseconds: 300_000_000)
    } catch {
        fputs("wrec-helper: error: \(error)\n", stderr)
        Foundation.exit(1)
    }
}

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

let semaphore = DispatchSemaphore(value: 0)
Task {
    await run()
    semaphore.signal()
}
semaphore.wait()
