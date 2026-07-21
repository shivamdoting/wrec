// Headless smoke test: `WREC_SMOKE=1 wrec-app` exercises the entire daemon
// wire — spawn, status, permissions, targets, start → poll → stop — and
// exits nonzero on any failure. No UI, no TCC prompts beyond what the daemon
// itself triggers. Used by scripts and CI to prove the shell ⇄ engine
// contract without a display.

import Foundation

enum Smoke {
    static var requested: Bool {
        ProcessInfo.processInfo.environment["WREC_SMOKE"] == "1"
    }

    /// Every exit stops the daemon the run spawned; a leaked daemon would
    /// outlive CI jobs and shadow later runs on the same WREC_HOME.
    private static func finish(_ code: Int32, _ daemon: DaemonClient) async -> Never {
        do {
            try await daemon.stopDaemon()
        } catch {
            print("smoke: warning: daemon left running (stop failed: \(error))")
        }
        exit(code)
    }

    static func run() async -> Never {
        setbuf(stdout, nil)
        let daemon = DaemonClient()
        do {
            try await daemon.ensure()
            let status = try await daemon.status()
            print("smoke: daemon \(status.daemonVersion) protocol \(status.protocolVersion)")

            let permission = try await daemon.screenPermissionStatus()
            print("smoke: screen permission \(permission.rawValue)")

            guard permission.isGranted else {
                print("smoke: PASS (stopping before record: no screen permission)")
                await finish(0, daemon)
            }

            let targets = try await daemon.listTargets()
            print("smoke: \(targets.count) targets")
            guard let display = targets.first(where: { $0.kind == .display }) else {
                print("smoke: PASS (no display target)")
                await finish(0, daemon)
            }

            var settings = RecorderSettings.defaults()
            settings.outputDir = FileManager.default.temporaryDirectory
                .appending(path: "wrec-smoke").path
            let job = try await daemon.startRecording(
                StartRecordingParams(target: display, settings: settings))
            print("smoke: job \(job.id) \(job.status.rawValue)")

            try await Task.sleep(for: .seconds(2))
            let live = try await daemon.showJob(job.id)
            print("smoke: polled \(live.status.rawValue) events=\(live.events.count)")

            let stopped = try await daemon.stopJob(job.id)
            print("smoke: stop → \(stopped.status.rawValue)")

            for _ in 0..<20 {
                let snapshot = try await daemon.showJob(job.id)
                if snapshot.status.isTerminal {
                    print(
                        "smoke: terminal \(snapshot.status.rawValue) output=\(snapshot.outputPath ?? "-")"
                    )
                    guard snapshot.status == .completed else {
                        print("smoke: FAIL (job did not complete)")
                        await finish(1, daemon)
                    }
                    print("smoke: PASS")
                    await finish(0, daemon)
                }
                try await Task.sleep(for: .milliseconds(500))
            }
            print("smoke: FAIL (job never reached terminal state)")
            await finish(1, daemon)
        } catch {
            print("smoke: FAIL \(error)")
            await finish(1, daemon)
        }
    }
}
