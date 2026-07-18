// The one observable model behind the whole UI.
//
// Performance contract:
// - `@Observable` gives per-property invalidation: a metrics tick re-renders
//   only the metrics text, not the pickers; a settings change never touches
//   the transport views.
// - Zero timers at idle. The 500ms job poll is a structured task that exists
//   only between `record.start` and a terminal status, then vanishes.
// - Every daemon round trip happens inside the `DaemonClient` actor; the main
//   thread only ever assigns already-decoded value types.
// - Display strings (menu bar title, metrics line) are precomputed here once
//   per state change so view bodies do string interpolation exactly never.

import AppKit
import AVFoundation
import Foundation
import Observation

enum RecorderPhase: Equatable, Sendable {
    case idle
    case loadingTargets
    case starting
    case recording
    case pausing
    case paused
    case resuming
    case stopping
    case failed(String)

    var isActiveSession: Bool {
        switch self {
        case .starting, .recording, .pausing, .paused, .resuming, .stopping: true
        default: false
        }
    }

    var isBusy: Bool {
        switch self {
        case .starting, .pausing, .resuming, .stopping, .loadingTargets: true
        default: false
        }
    }
}

@MainActor
@Observable
final class RecorderModel {
    // State the UI observes.
    private(set) var phase: RecorderPhase = .idle
    private(set) var targets: [CaptureTarget] = []
    private(set) var metrics: RecorderMetrics?
    private(set) var screenPermission: PermissionStatus = .unknown
    private(set) var micPermission: PermissionStatus = .unknown
    private(set) var lastRecordingDir: URL?
    private(set) var toast: String?
    var settings: RecorderSettings
    var selectedTargetKey: String?
    var showNerdLogs: Bool

    /// Menu bar title, empty at idle. Precomputed; changes at most once/second.
    private(set) var menuBarText: String = ""
    /// `0:42  12.3 MB  4.1 Mbps` while recording.
    private(set) var metricsText: String = ""

    @ObservationIgnored private let daemon = DaemonClient()
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var activeJobId: UInt64?
    @ObservationIgnored private var toastDismissTask: Task<Void, Never>?

    init() {
        let config = ConfigStore.load()
        settings = config.settings
        selectedTargetKey = config.selectedTargetKey
        showNerdLogs = config.showNerdLogs

        Task { await bootstrap() }
    }

    var visibleTargets: [CaptureTarget] {
        targets.filter { $0.kind == settings.source }
    }

    /// Selection is always within the current source kind — switching
    /// Display/Window must never leave a hidden target armed.
    var selectedTarget: CaptureTarget? {
        let visible = visibleTargets
        guard let key = selectedTargetKey else { return visible.first }
        return visible.first { $0.key == key } ?? visible.first
    }

    var canRecord: Bool {
        screenPermission.isGranted && selectedTarget != nil && !phase.isBusy
            && (!settings.includeMicrophone || micPermission.isGranted)
    }

    // MARK: - Bootstrap

    private func bootstrap() async {
        await refreshMicPermission()
        await refreshScreenPermission(requestIfNeeded: false)
        if screenPermission.isGranted {
            await refreshTargets()
        }
        await adoptRunningJob()
    }

    /// If a daemon is already recording (e.g. started from the CLI, or the
    /// app relaunched mid-session), attach to it instead of pretending idle.
    private func adoptRunningJob() async {
        guard let status = try? await daemon.status(), let jobId = status.activeJobId else { return }
        activeJobId = jobId
        startPolling(jobId)
    }

    // MARK: - Permissions

    func refreshScreenPermission(requestIfNeeded: Bool) async {
        do {
            try await daemon.ensure()
            let status =
                requestIfNeeded
                ? try await daemon.requestScreenPermission()
                : try await daemon.screenPermissionStatus()
            screenPermission = status
            if status.isGranted, targets.isEmpty {
                await refreshTargets()
            }
        } catch {
            screenPermission = .unknown
            show(toast: "\(error)")
        }
    }

    func refreshMicPermission() async {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: micPermission = .granted
        case .notDetermined: micPermission = .unknown
        default: micPermission = .missing
        }
    }

    func requestMicPermission() async {
        let granted = await AVCaptureDevice.requestAccess(for: .audio)
        micPermission = granted ? .granted : .missing
        if !granted {
            NSWorkspace.shared.open(
                URL(
                    string:
                        "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
                )!)
        }
    }

    // MARK: - Targets

    func refreshTargets() async {
        guard screenPermission.isGranted else { return }
        if phase == .idle { phase = .loadingTargets }
        do {
            try await daemon.ensure()
            targets = try await daemon.listTargets()
        } catch {
            show(toast: "\(error)")
        }
        if phase == .loadingTargets { phase = .idle }
    }

    // MARK: - Transport

    func toggleRecording() {
        if let jobId = activeJobId {
            transition(to: .stopping) { [daemon] in try await daemon.stopJob(jobId) }
            return
        }
        guard let target = selectedTarget, canRecord else { return }
        let params = StartRecordingParams(target: target, settings: settings)
        phase = .starting
        Task {
            do {
                try await daemon.ensure()
                let job = try await daemon.startRecording(params)
                activeJobId = job.id
                apply(job)
                startPolling(job.id)
            } catch {
                phase = .failed("\(error)")
                show(toast: "\(error)")
            }
        }
    }

    func togglePause() {
        guard let jobId = activeJobId else { return }
        if phase == .paused {
            transition(to: .resuming) { [daemon] in try await daemon.resumeJob(jobId) }
        } else if phase == .recording {
            transition(to: .pausing) { [daemon] in try await daemon.pauseJob(jobId) }
        }
    }

    private func transition(
        to phase: RecorderPhase,
        _ op: @escaping @Sendable () async throws -> JobSnapshot
    ) {
        let previous = self.phase
        self.phase = phase
        Task {
            do {
                apply(try await op())
            } catch {
                // Roll the optimistic phase back so the transport doesn't
                // stay disabled after a failed pause/resume/stop.
                if self.phase == phase { self.phase = previous }
                show(toast: "\(error)")
            }
        }
    }

    // MARK: - Poll loop (exists only while a job is live)

    private func startPolling(_ jobId: UInt64) {
        pollTask?.cancel()
        pollTask = Task { [daemon] in
            // ~5s of consecutive failures means the daemon died or dropped
            // the job; fail the session instead of spinning forever.
            var consecutiveFailures = 0
            while !Task.isCancelled {
                guard let job = try? await daemon.showJob(jobId) else {
                    consecutiveFailures += 1
                    if consecutiveFailures >= 10 {
                        self.finishSession()
                        self.phase = .failed("lost contact with the recording daemon")
                        self.show(toast: "Lost contact with the recording daemon")
                        return
                    }
                    try? await Task.sleep(for: .milliseconds(500))
                    continue
                }
                consecutiveFailures = 0
                self.apply(job)
                if job.status.isTerminal { break }
                try? await Task.sleep(for: .milliseconds(500))
            }
        }
    }

    private func apply(_ job: JobSnapshot) {
        // Metrics first: terminal branches below clear the session (including
        // metrics), and nothing may repopulate them afterwards or the next
        // recording briefly flashes the previous session's numbers.
        if let latest = job.latestMetrics, latest != metrics {
            metrics = latest
            metricsText = Self.format(metrics: latest)
        }

        switch job.status {
        case .queued, .starting: phase = .starting
        case .recording: phase = .recording
        case .paused: phase = .paused
        case .finishing: phase = .stopping
        case .completed:
            finishSession()
            if let path = job.outputPath {
                let url = URL(fileURLWithPath: path)
                lastRecordingDir = url.deletingLastPathComponent()
                show(toast: "Saved \(url.lastPathComponent)")
            }
        case .failed:
            finishSession()
            let message = job.latestErrorMessage ?? "recording failed"
            phase = .failed(message)
            show(toast: message)
        case .cancelled:
            finishSession()
        }

        menuBarText = Self.menuBarText(phase: phase, metrics: metrics)
    }

    private func finishSession() {
        pollTask?.cancel()
        pollTask = nil
        activeJobId = nil
        phase = .idle
        metrics = nil
        metricsText = ""
        menuBarText = ""
    }

    // MARK: - Settings mutations (each persists exactly once)

    func update(_ mutate: (inout RecorderSettings) -> Void) {
        mutate(&settings)
        settings.applyPresetLimits()
        persist()
    }

    func select(targetKey: String) {
        selectedTargetKey = targetKey
        persist()
    }

    func setShowNerdLogs(_ value: Bool) {
        showNerdLogs = value
        persist()
    }

    private func persist() {
        let config = AppConfig(
            settings: settings,
            selectedTargetKey: selectedTargetKey,
            showNerdLogs: showNerdLogs
        )
        Task.detached(priority: .utility) { ConfigStore.save(config) }
    }

    // MARK: - Quit

    /// Stop any live job first and give the daemon a moment to reach a
    /// terminal state so the file is finalized, then quit regardless — the
    /// user asked to leave, and the daemon is a separate process.
    func quit() {
        guard let jobId = activeJobId else {
            NSApp.terminate(nil)
            return
        }
        phase = .stopping
        Task {
            _ = try? await daemon.stopJob(jobId)
            for _ in 0..<8 {
                if let job = try? await daemon.showJob(jobId), job.status.isTerminal { break }
                try? await Task.sleep(for: .milliseconds(500))
            }
            NSApp.terminate(nil)
        }
    }

    // MARK: - Toasts

    func show(toast message: String) {
        toast = message
        toastDismissTask?.cancel()
        toastDismissTask = Task {
            try? await Task.sleep(for: .seconds(4))
            if !Task.isCancelled { toast = nil }
        }
    }

    // MARK: - Formatting (precomputed, views never interpolate)

    private static func format(metrics: RecorderMetrics) -> String {
        let mb = Double(metrics.outputBytes) / 1_000_000
        var text = String(
            format: "%d:%02d  %.1f MB  %.1f Mbps",
            metrics.elapsedSecs / 60, metrics.elapsedSecs % 60,
            mb, metrics.estimatedBitrateMbps
        )
        if let frames = metrics.frames {
            text += String(format: "  %d f", frames)
            if let dropped = metrics.droppedFrames, dropped > 0 {
                text += String(format: "  %d dropped", dropped)
            }
        }
        return text
    }

    private static func menuBarText(phase: RecorderPhase, metrics: RecorderMetrics?) -> String {
        switch phase {
        case .recording:
            guard let metrics else { return "REC" }
            return String(
                format: "%d:%02d", metrics.elapsedSecs / 60, metrics.elapsedSecs % 60)
        case .paused: return "II"
        case .starting, .stopping, .pausing, .resuming: return "…"
        default: return ""
        }
    }
}
