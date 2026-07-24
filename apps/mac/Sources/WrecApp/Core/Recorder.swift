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
import CoreFoundation
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
    var autoOpenAfterRecording: Bool

    /// Live recording time, frozen while the completed file is being finalized.
    private(set) var menuBarText: String = ""
    /// `12.3 MB  4.1 Mbps  1260 f` while recording.
    private(set) var metricsText: String = ""

    @ObservationIgnored private let daemon = DaemonClient()
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var activeJobId: UInt64?
    @ObservationIgnored private var stoppingMenuBarText: String?
    @ObservationIgnored private var toastDismissTask: Task<Void, Never>?
    @ObservationIgnored private var lastTargetsRefresh: Date = .distantPast
    @ObservationIgnored private var targetsRefreshInFlight = false
    @ObservationIgnored private var screenWatcher: Task<Void, Never>?

    init() {
        let config = ConfigStore.load()
        settings = config.settings
        selectedTargetKey = config.selectedTargetKey
        showNerdLogs = config.showNerdLogs
        autoOpenAfterRecording = config.autoOpenAfterRecording

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
        // Passive: an observer costs nothing at idle. It's the only way the
        // app learns about a job it didn't start itself (e.g. from the CLI),
        // since there is deliberately no idle polling to notice one.
        observeJobChangedNotifications()

        await refreshMicPermission()
        // When granted, this also runs the first target sweep (they're
        // empty at launch) — no second one needed here.
        await refreshScreenPermission(requestIfNeeded: false)
        await adoptRunningJob()

        // Displays are the one target class that changes without the user
        // touching wrec; refresh on plug/unplug instead of on every open.
        screenWatcher = Task { [weak self] in
            let changes = NotificationCenter.default
                .notifications(named: NSApplication.didChangeScreenParametersNotification)
                .map { _ in () }
            for await _ in changes {
                await self?.refreshTargets()
            }
        }
    }

    /// The daemon posts this distributed notification (no payload) after
    /// every job transition. The C callback can't capture `self`, so the
    /// observer pointer carries it across, and it hops back onto the
    /// MainActor before touching model state.
    private func observeJobChangedNotifications() {
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDistributedCenter(),
            Unmanaged.passUnretained(self).toOpaque(),
            { _, observer, _, _, _ in
                guard let observer else { return }
                let model = Unmanaged<RecorderModel>.fromOpaque(observer).takeUnretainedValue()
                Task { @MainActor in
                    await model.adoptRunningJob()
                }
            },
            Self.jobChangedNotificationName as CFString,
            nil,
            .deliverImmediately
        )
    }

    // A plain String so the nonisolated deinit can reach it too.
    private nonisolated static let jobChangedNotificationName = "app.wrec.job-changed"

    /// The production model lives for the whole process, but anything that
    /// creates a short-lived instance (tests) must not leave the center
    /// holding a dangling observer pointer.
    deinit {
        CFNotificationCenterRemoveObserver(
            CFNotificationCenterGetDistributedCenter(),
            Unmanaged.passUnretained(self).toOpaque(),
            CFNotificationName(Self.jobChangedNotificationName as CFString),
            nil
        )
    }

    /// If a daemon is already recording (e.g. started from the CLI, or the
    /// app relaunched mid-session), attach to it instead of pretending idle.
    /// Keyed on the daemon's job id, not just "is one tracked": back-to-back
    /// CLI recordings can start a new job before the old job's poll loop has
    /// seen its terminal snapshot, and a guard on the stale id would drop the
    /// new job's only notification. Same-id deliveries stay no-ops.
    func adoptRunningJob() async {
        guard let status = try? await daemon.status(), let jobId = status.activeJobId else { return }
        guard jobId != activeJobId else { return }
        if let outgoing = activeJobId {
            // Hand the session over; the old poll loop would otherwise keep
            // painting a dead job's timer over the new one's. Its terminal
            // snapshot still owes the user the Saved toast (and auto-open),
            // so deliver that before tearing the session down.
            if let final = try? await daemon.showJob(outgoing), final.status.isTerminal {
                apply(final)
            } else {
                finishSession()
            }
        }
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
        // A persisted mic preference without the permission behind it (revoked
        // in System Settings, config migrated to another machine) would render
        // the toggle off — making the clearing branch unreachable — while
        // `canRecord` stays false. Reconcile so config never outruns TCC.
        if !micPermission.isGranted && settings.includeMicrophone {
            update { $0.includeMicrophone = false }
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
        // Bootstrap and the permission refresh race to populate an empty
        // list; one engine sweep is enough (MainActor makes this race-free).
        guard !targetsRefreshInFlight else { return }
        targetsRefreshInFlight = true
        defer { targetsRefreshInFlight = false }
        // Busy only when there is nothing to render yet. A refresh over a
        // cached list swaps the contents in place; entering a busy phase
        // here made every popover open flicker disabled → enabled.
        if targets.isEmpty, phase == .idle { phase = .loadingTargets }
        do {
            try await daemon.ensure()
            targets = try await daemon.listTargets()
            lastTargetsRefresh = Date()
        } catch {
            show(toast: "\(error)")
        }
        if phase == .loadingTargets { phase = .idle }
    }

    /// Freshness without per-open cost: window lists rot fast, display
    /// lists basically never. Called on popover open and on switching the
    /// source to Window; the TTL keeps segment toggling and rapid re-opens
    /// from spawning a capture engine each time.
    func refreshTargetsIfStale() async {
        guard settings.source == .window else { return }
        guard Date().timeIntervalSince(lastTargetsRefresh) > 15 else { return }
        await refreshTargets()
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
        stoppingMenuBarText = nil
        menuBarText = Self.menuBarText(phase: phase, metrics: metrics)
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
        if phase == .stopping {
            let frozen = Self.menuBarText(phase: .recording, metrics: metrics)
            stoppingMenuBarText = frozen
            menuBarText = frozen
        } else {
            menuBarText = Self.menuBarText(phase: phase, metrics: metrics)
        }
        Task {
            do {
                apply(try await op())
            } catch {
                // Roll the optimistic phase back so the transport doesn't
                // stay disabled after a failed pause/resume/stop.
                if self.phase == phase {
                    self.phase = previous
                    self.stoppingMenuBarText = nil
                    self.menuBarText = Self.menuBarText(phase: previous, metrics: self.metrics)
                }
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
                // A cancel that landed while `showJob` was in flight must not
                // deliver a stale snapshot (it could flip .stopping back to
                // .recording after the session was torn down).
                if Task.isCancelled { return }
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
                let directory = url.deletingLastPathComponent()
                lastRecordingDir = directory
                show(toast: "Saved \(url.lastPathComponent)")
                if autoOpenAfterRecording {
                    Platform.open(directory)
                }
            }
        case .failed:
            finishSession()
            let message = job.latestErrorMessage ?? "recording failed"
            phase = .failed(message)
            show(toast: message)
        case .cancelled:
            finishSession()
        }

        if !job.status.isTerminal {
            if phase == .stopping {
                if stoppingMenuBarText == nil {
                    stoppingMenuBarText = Self.menuBarText(phase: .recording, metrics: metrics)
                }
                menuBarText = stoppingMenuBarText ?? ""
            } else {
                menuBarText = Self.menuBarText(phase: phase, metrics: metrics)
            }
        }
    }

    private func finishSession() {
        pollTask?.cancel()
        pollTask = nil
        activeJobId = nil
        stoppingMenuBarText = nil
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

    func setAutoOpenAfterRecording(_ value: Bool) {
        autoOpenAfterRecording = value
        persist()
    }

    private func persist() {
        let config = AppConfig(
            settings: settings,
            selectedTargetKey: selectedTargetKey,
            showNerdLogs: showNerdLogs,
            autoOpenAfterRecording: autoOpenAfterRecording
        )
        ConfigStore.save(config)
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
        pollTask?.cancel()
        pollTask = nil
        phase = .stopping
        // Use a separate short-timeout client so a cancelled in-flight poll
        // cannot leave quit queued behind the normal 10-second socket timeout.
        let quitDaemon = DaemonClient(timeoutSeconds: 1)
        Task { [quitDaemon] in
            let deadline = ContinuousClock.now.advanced(by: .seconds(5))
            var consecutiveFailures = 0
            _ = try? await quitDaemon.stopJob(jobId)
            while ContinuousClock.now < deadline {
                if let job = try? await quitDaemon.showJob(jobId) {
                    consecutiveFailures = 0
                    if job.status.isTerminal { break }
                } else {
                    consecutiveFailures += 1
                    if consecutiveFailures >= 4 { break }
                }
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
        var text = String(format: "%.1f MB  %.1f Mbps", mb, Double(metrics.estimatedBitrateMbps))
        if let frames = metrics.frames {
            text += String(format: "  %d f", frames)
        }
        return text
    }

    private static func menuBarText(phase: RecorderPhase, metrics: RecorderMetrics?) -> String {
        switch phase {
        case .recording, .pausing, .paused, .resuming:
            guard let metrics else { return "REC" }
            return String(
                format: "%d:%02d", metrics.elapsedSecs / 60, metrics.elapsedSecs % 60)
        case .stopping: return ""
        case .starting: return "…"
        default: return ""
        }
    }
}
