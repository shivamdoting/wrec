// Wire-compatible mirrors of the Rust `domain` and `control` types.
//
// The daemon speaks compact JSON with snake_case keys (serde). Everything here
// is a value type — decoded once per IPC round-trip, then handed to the UI as
// immutable data. Coding strategies convert snake_case; enum raw values match
// serde's rename attributes exactly (`FrameRate::Fps30` ⇄ "30", etc.).

import Foundation

// MARK: - Domain enums (raw values are the serde-serialized names)

enum CaptureSourceKind: String, Codable, CaseIterable, Sendable {
    case display, window

    init(from decoder: Decoder) throws {
        self = try decodeRawValue(
            from: decoder,
            aliases: ["Display": .display, "Window": .window]
        )
    }

    var label: String { self == .display ? "Display" : "Window" }
}

enum Codec: String, Codable, CaseIterable, Sendable {
    case hevc, h264

    init(from decoder: Decoder) throws {
        self = try decodeRawValue(from: decoder, aliases: ["Hevc": .hevc, "H264": .h264])
    }

    var label: String { self == .hevc ? "HEVC" : "H.264" }
}

enum FrameRate: String, Codable, CaseIterable, Sendable {
    case fps30 = "30"
    case fps60 = "60"

    init(from decoder: Decoder) throws {
        self = try decodeRawValue(
            from: decoder,
            aliases: ["Fps30": .fps30, "Fps60": .fps60]
        )
    }

    var label: String { rawValue + " FPS" }
}

enum Quality: String, Codable, CaseIterable, Sendable {
    case efficient, balanced, high

    init(from decoder: Decoder) throws {
        self = try decodeRawValue(
            from: decoder,
            aliases: ["Efficient": .efficient, "Balanced": .balanced, "High": .high]
        )
    }

    var label: String { rawValue.capitalized }

    /// Preset ceilings, mirrored from `RecorderSettings::with_preset_limits`.
    var maxResolution: Resolution? {
        switch self {
        case .efficient: .r720p
        case .balanced: .r1080p
        case .high: nil
        }
    }

    var maxFps: FrameRate {
        self == .high ? .fps60 : .fps30
    }
}

enum Resolution: String, Codable, CaseIterable, Sendable {
    case native
    case r720p = "720p"
    case r1080p = "1080p"
    case r2k = "2k"
    case r4k = "4k"

    init(from decoder: Decoder) throws {
        self = try decodeRawValue(
            from: decoder,
            aliases: [
                "Native": .native,
                "R720p": .r720p,
                "R1080p": .r1080p,
                "R2k": .r2k,
                "R4k": .r4k,
            ]
        )
    }

    var label: String {
        switch self {
        case .native: "Original"
        case .r720p: "720p"
        case .r1080p: "1080p"
        case .r2k: "2K"
        case .r4k: "4K"
        }
    }

    /// Rank for preset capping: anything above the cap is disallowed.
    var rank: Int {
        switch self {
        case .r720p: 0
        case .r1080p: 1
        case .r2k: 2
        case .r4k: 3
        case .native: 4
        }
    }
}

private func decodeRawValue<Value>(
    from decoder: Decoder,
    aliases: [String: Value]
) throws -> Value where Value: RawRepresentable, Value.RawValue == String {
    let container = try decoder.singleValueContainer()
    let rawValue = try container.decode(String.self)
    if let value = Value(rawValue: rawValue) ?? aliases[rawValue] { return value }
    throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "Invalid \(Value.self) value: \(rawValue)"
    )
}

enum PermissionStatus: String, Codable, Sendable {
    case unknown, granted, missing

    var isGranted: Bool { self == .granted }
}

struct CaptureTarget: Codable, Hashable, Sendable, Identifiable {
    let id: UInt64
    let name: String
    let kind: CaptureSourceKind

    /// Stable key persisted in config: `display:1` / `window:42`.
    var key: String { "\(kind.rawValue):\(id)" }
}

// MARK: - Settings / config (persisted at Application Support/Wrec/config.json)

struct RecorderSettings: Codable, Equatable, Sendable {
    var source: CaptureSourceKind
    var fps: FrameRate
    var codec: Codec
    var quality: Quality
    var resolution: Resolution
    var outputDir: String
    var includeCursor: Bool
    var includeSystemAudio: Bool
    var includeMicrophone: Bool
    var hideWrec: Bool
    var showMicIndicator: Bool

    enum CodingKeys: String, CodingKey {
        case source, fps, codec, quality, resolution
        case outputDir, includeCursor, includeSystemAudio, includeMicrophone
        case hideWrec, showMicIndicator
    }

    // The custom Decodable init below suppresses the synthesized memberwise
    // initializer; `defaults()` needs it back.
    init(
        source: CaptureSourceKind, fps: FrameRate, codec: Codec, quality: Quality,
        resolution: Resolution, outputDir: String, includeCursor: Bool,
        includeSystemAudio: Bool, includeMicrophone: Bool, hideWrec: Bool,
        showMicIndicator: Bool
    ) {
        self.source = source
        self.fps = fps
        self.codec = codec
        self.quality = quality
        self.resolution = resolution
        self.outputDir = outputDir
        self.includeCursor = includeCursor
        self.includeSystemAudio = includeSystemAudio
        self.includeMicrophone = includeMicrophone
        self.hideWrec = hideWrec
        self.showMicIndicator = showMicIndicator
    }

    /// Mirrors the Rust struct's `#[serde(default)]` fields so configs
    /// written by older versions still load instead of being discarded.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        source = try container.decode(CaptureSourceKind.self, forKey: .source)
        fps = try container.decode(FrameRate.self, forKey: .fps)
        codec = try container.decode(Codec.self, forKey: .codec)
        quality = try container.decode(Quality.self, forKey: .quality)
        resolution =
            try container.decodeIfPresent(Resolution.self, forKey: .resolution) ?? .r1080p
        outputDir = try container.decode(String.self, forKey: .outputDir)
        includeCursor = try container.decode(Bool.self, forKey: .includeCursor)
        includeSystemAudio =
            try container.decodeIfPresent(Bool.self, forKey: .includeSystemAudio) ?? true
        includeMicrophone =
            try container.decodeIfPresent(Bool.self, forKey: .includeMicrophone) ?? false
        hideWrec = try container.decodeIfPresent(Bool.self, forKey: .hideWrec) ?? true
        showMicIndicator =
            try container.decodeIfPresent(Bool.self, forKey: .showMicIndicator) ?? true
    }

    static func defaults() -> RecorderSettings {
        RecorderSettings(
            source: .display,
            fps: .fps30,
            codec: .hevc,
            quality: .balanced,
            resolution: .r1080p,
            outputDir: WrecPaths.defaultOutputDir().path,
            includeCursor: true,
            includeSystemAudio: true,
            includeMicrophone: false,
            hideWrec: true,
            showMicIndicator: true
        )
    }

    /// Mirror of `with_preset_limits`: clamp resolution/fps to the preset cap.
    mutating func applyPresetLimits() {
        if let cap = quality.maxResolution, resolution.rank > cap.rank { resolution = cap }
        if !fpsAllowed(fps) { fps = .fps30 }
    }

    func resolutionAllowed(_ resolution: Resolution) -> Bool {
        guard let cap = quality.maxResolution else { return true }
        return resolution.rank <= cap.rank
    }

    func fpsAllowed(_ fps: FrameRate) -> Bool {
        fps == .fps30 || quality.maxFps == .fps60
    }
}

struct AppConfig: Codable, Sendable {
    var settings: RecorderSettings
    var selectedTargetKey: String?
    var showNerdLogs: Bool
    var autoOpenAfterRecording: Bool

    enum CodingKeys: String, CodingKey {
        case settings
        case selectedTargetKey
        case showNerdLogs
        case autoOpenAfterRecording
    }

    init(
        settings: RecorderSettings,
        selectedTargetKey: String?,
        showNerdLogs: Bool,
        autoOpenAfterRecording: Bool
    ) {
        self.settings = settings
        self.selectedTargetKey = selectedTargetKey
        self.showNerdLogs = showNerdLogs
        self.autoOpenAfterRecording = autoOpenAfterRecording
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        settings = try container.decode(RecorderSettings.self, forKey: .settings)
        selectedTargetKey = try container.decodeIfPresent(String.self, forKey: .selectedTargetKey)
        showNerdLogs = try container.decodeIfPresent(Bool.self, forKey: .showNerdLogs) ?? false
        autoOpenAfterRecording =
            try container.decodeIfPresent(Bool.self, forKey: .autoOpenAfterRecording) ?? false
    }
}

// MARK: - IPC protocol types

struct AgentError: Codable, Error, Sendable, CustomStringConvertible {
    let code: String
    let message: String
    let recoverable: Bool
    let next: String

    var description: String { message }
}

struct AgentWarning: Codable, Sendable {
    let code: String
    let message: String
    let next: String
}

enum JobStatus: String, Codable, Sendable {
    case queued, starting, recording, paused, finishing, completed, failed, cancelled

    var isTerminal: Bool {
        switch self {
        case .completed, .failed, .cancelled: true
        default: false
        }
    }
}

enum EventLevel: String, Codable, Sendable {
    case info, warning, error
}

struct RecorderMetrics: Codable, Sendable, Equatable {
    let elapsedSecs: UInt64
    let outputBytes: UInt64
    let estimatedBitrateMbps: Float
    let frames: UInt64?
    let droppedFrames: UInt64?
}

struct JobEvent: Codable, Sendable {
    let timestampMs: UInt64
    let level: EventLevel
    let message: String
    let metrics: RecorderMetrics?
}

struct JobSnapshot: Codable, Sendable {
    let id: UInt64
    let status: JobStatus
    let target: CaptureTarget?
    let outputPath: String?
    let queuedPosition: Int?
    let warnings: [AgentWarning]
    let events: [JobEvent]

    var latestMetrics: RecorderMetrics? {
        for event in events.reversed() {
            if let metrics = event.metrics { return metrics }
        }
        return nil
    }

    var latestErrorMessage: String? {
        for event in events.reversed() where event.level == .error {
            return event.message
        }
        return nil
    }
}

/// `TargetSelector::Id` — the only variant the GUI sends.
/// Serializes as `{"type":"id","kind":"display","id":1}`.
struct TargetSelectorId: Encodable, Sendable {
    let kind: CaptureSourceKind
    let id: UInt64

    enum CodingKeys: String, CodingKey { case type, kind, id }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode("id", forKey: .type)
        try container.encode(kind, forKey: .kind)
        try container.encode(id, forKey: .id)
    }
}

struct RecordingOptions: Encodable, Sendable {
    let sourceKind: CaptureSourceKind
    let fps: FrameRate
    let codec: Codec
    let quality: Quality
    let resolution: Resolution
    let outputDir: String
    let includeCursor: Bool
    let includeSystemAudio: Bool
    let includeMicrophone: Bool
    let hideWrec: Bool
    /// The GUI always suppresses the engine's floating mic pill, matching the
    /// previous app: the shell surfaces mic state itself.
    let showMicIndicator: Bool

    init(settings: RecorderSettings) {
        sourceKind = settings.source
        fps = settings.fps
        codec = settings.codec
        quality = settings.quality
        resolution = settings.resolution
        outputDir = settings.outputDir
        includeCursor = settings.includeCursor
        includeSystemAudio = settings.includeSystemAudio
        includeMicrophone = settings.includeMicrophone
        hideWrec = settings.hideWrec
        showMicIndicator = false
    }
}

struct StartRecordingParams: Encodable, Sendable {
    let selector: TargetSelectorId
    let options: RecordingOptions
    let durationMs: UInt64?
    let queue: Bool

    init(target: CaptureTarget, settings: RecorderSettings) {
        selector = TargetSelectorId(kind: target.kind, id: target.id)
        options = RecordingOptions(settings: settings)
        durationMs = nil
        queue = false
    }
}

// MARK: - Result payloads

struct DaemonStatus: Decodable, Sendable {
    let daemonVersion: String
    let protocolVersion: Int
    let activeJobId: UInt64?
}

struct TargetsResult: Decodable, Sendable {
    let targets: [CaptureTarget]
}

struct PermissionResult: Decodable, Sendable {
    let status: PermissionStatus
}

struct JobResult: Decodable, Sendable {
    let job: JobSnapshot
}

struct EmptyParams: Encodable, Sendable {}

struct JobIdParams: Encodable, Sendable {
    let jobId: UInt64
}
