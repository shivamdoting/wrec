import AVFoundation
import CoreMedia
import CoreVideo
import Foundation

let canonicalWidth = 1280.0
let canonicalHeight = 720.0
let markerBlockSize = 24.0
let markerBlockHeight = 64.0
let markerBitCount = 32
let markerGuardCount = 4
let markerY = 632.0
let guardPrefix = [true, false, true, false]
let guardSuffix = [false, true, false, true]

struct Dimensions: Encodable {
    let width: Int
    let height: Int
}

struct DecodedFrame: Encodable {
    let ptsMs: Double
    let stimulusIndex: UInt32?
}

struct DecodeResult: Encodable {
    let codec: String
    let dimensions: Dimensions
    let durationMs: Double
    let frames: [DecodedFrame]
}

enum DecodeError: Error, CustomStringConvertible {
    case usage
    case noVideoTrack
    case readerCreation(String)
    case readerStart(String)

    var description: String {
        switch self {
        case .usage:
            "usage: decode <movie.mov>"
        case .noVideoTrack:
            "movie does not contain a video track"
        case .readerCreation(let message):
            "could not create asset reader: \(message)"
        case .readerStart(let message):
            "could not start asset reader: \(message)"
        }
    }
}

func main() throws {
    guard CommandLine.arguments.count == 2 else {
        throw DecodeError.usage
    }

    let url = URL(fileURLWithPath: CommandLine.arguments[1])
    let asset = AVURLAsset(url: url)
    guard let track = asset.tracks(withMediaType: .video).first else {
        throw DecodeError.noVideoTrack
    }

    let reader: AVAssetReader
    do {
        reader = try AVAssetReader(asset: asset)
    } catch {
        throw DecodeError.readerCreation(error.localizedDescription)
    }

    let output = AVAssetReaderTrackOutput(
        track: track,
        outputSettings: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
    )
    output.alwaysCopiesSampleData = false
    reader.add(output)

    guard reader.startReading() else {
        throw DecodeError.readerStart(reader.error?.localizedDescription ?? "unknown error")
    }

    var frames: [DecodedFrame] = []
    var decodedDimensions: Dimensions?

    while let sample = output.copyNextSampleBuffer() {
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        guard let imageBuffer = CMSampleBufferGetImageBuffer(sample) else {
            continue
        }

        let width = CVPixelBufferGetWidth(imageBuffer)
        let height = CVPixelBufferGetHeight(imageBuffer)
        if decodedDimensions == nil {
            decodedDimensions = Dimensions(width: width, height: height)
        }

        let index = decodeStimulusIndex(from: imageBuffer)
        frames.append(
            DecodedFrame(
                ptsMs: pts.isValid ? pts.seconds * 1000.0 : 0,
                stimulusIndex: index
            )
        )
    }

    let natural = track.naturalSize.applying(track.preferredTransform)
    let dimensions = decodedDimensions ?? Dimensions(
        width: max(0, Int(abs(natural.width).rounded())),
        height: max(0, Int(abs(natural.height).rounded()))
    )
    let result = DecodeResult(
        codec: codecName(track: track),
        dimensions: dimensions,
        durationMs: asset.duration.seconds * 1000.0,
        frames: frames
    )
    let data = try JSONEncoder().encode(result)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

func decodeStimulusIndex(from pixelBuffer: CVPixelBuffer) -> UInt32? {
    CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }

    guard let base = CVPixelBufferGetBaseAddress(pixelBuffer) else {
        return nil
    }

    let width = CVPixelBufferGetWidth(pixelBuffer)
    let height = CVPixelBufferGetHeight(pixelBuffer)
    let bytesPerRow = CVPixelBufferGetBytesPerRow(pixelBuffer)
    let bytes = base.assumingMemoryBound(to: UInt8.self)
    let totalBlocks = markerBitCount + markerGuardCount * 2
    let scaleX = Double(width) / canonicalWidth
    let scaleY = Double(height) / canonicalHeight
    let startX = (canonicalWidth - Double(totalBlocks) * markerBlockSize) / 2.0
    let sampleY = Int(((markerY + markerBlockHeight / 2.0) * scaleY).rounded())

    var blocks: [Bool] = []
    for block in 0..<totalBlocks {
        let sampleX = Int((startX + (Double(block) + 0.5) * markerBlockSize) * scaleX)
        blocks.append(sampleBlock(bytes: bytes, width: width, height: height, bytesPerRow: bytesPerRow, x: sampleX, y: sampleY))
    }

    guard Array(blocks[0..<markerGuardCount]) == guardPrefix else {
        return nil
    }
    let suffixStart = markerGuardCount + markerBitCount
    guard Array(blocks[suffixStart..<suffixStart + markerGuardCount]) == guardSuffix else {
        return nil
    }

    var value: UInt32 = 0
    for bit in 0..<markerBitCount {
        if blocks[markerGuardCount + bit] {
            value |= 1 << UInt32(bit)
        }
    }
    return value
}

func sampleBlock(bytes: UnsafePointer<UInt8>, width: Int, height: Int, bytesPerRow: Int, x: Int, y: Int) -> Bool {
    let clampedX = min(max(x, 0), max(width - 1, 0))
    let clampedY = min(max(y, 0), max(height - 1, 0))
    let radius = 2
    var lumaTotal = 0.0
    var count = 0

    for yy in max(0, clampedY - radius)...min(height - 1, clampedY + radius) {
        for xx in max(0, clampedX - radius)...min(width - 1, clampedX + radius) {
            let offset = yy * bytesPerRow + xx * 4
            let blue = Double(bytes[offset])
            let green = Double(bytes[offset + 1])
            let red = Double(bytes[offset + 2])
            lumaTotal += 0.2126 * red + 0.7152 * green + 0.0722 * blue
            count += 1
        }
    }

    return count > 0 && (lumaTotal / Double(count)) >= 128.0
}

func codecName(track: AVAssetTrack) -> String {
    guard let description = track.formatDescriptions.first else {
        return "unknown"
    }
    let subtype = CMFormatDescriptionGetMediaSubType(description as! CMFormatDescription)
    let raw = fourCharCode(subtype)
    switch raw {
    case "hvc1", "hev1":
        return "hevc"
    case "avc1":
        return "h264"
    default:
        return raw
    }
}

func fourCharCode(_ value: FourCharCode) -> String {
    let chars = [
        UInt8((value >> 24) & 0xff),
        UInt8((value >> 16) & 0xff),
        UInt8((value >> 8) & 0xff),
        UInt8(value & 0xff),
    ]
    return String(bytes: chars, encoding: .macOSRoman) ?? "\(value)"
}

do {
    try main()
} catch {
    fputs("decode: \(error)\n", stderr)
    exit(1)
}
