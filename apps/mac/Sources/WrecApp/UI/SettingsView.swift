// The rare knobs: output location, permissions, CLI/skill install, updates.
// Native Settings scene, native Form styling — nothing custom to maintain.

import SwiftUI

struct SettingsView: View {
    @Bindable var model: RecorderModel

    var body: some View {
        TabView {
            GeneralTab(model: model)
                .tabItem { Label("General", systemImage: "gearshape") }
            PermissionsTab(model: model)
                .tabItem { Label("Permissions", systemImage: "lock.shield") }
            CliTab()
                .tabItem { Label("CLI", systemImage: "terminal") }
            AboutTab()
                .tabItem { Label("About", systemImage: "info.circle") }
        }
        .frame(width: 440)
        .controlSize(.small)
        .onAppear { NSApp.activate(ignoringOtherApps: true) }
    }
}

#if DEBUG
/// Hosts the General tab without the TabView chrome for `WREC_UI_PREVIEW=settings`.
struct SettingsGeneralPreview: View {
    let model: RecorderModel

    var body: some View { GeneralTab(model: model) }
}
#endif

// MARK: - General

private struct GeneralTab: View {
    @Bindable var model: RecorderModel

    var body: some View {
        Form {
            LabeledContent("Save recordings to") {
                HStack(spacing: 6) {
                    Text(model.settings.outputDir)
                        .truncationMode(.middle)
                        .lineLimit(1)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: 118, alignment: .trailing)
                        .help(model.settings.outputDir)
                    Button("Choose…") {
                        Task {
                            if let url = await Platform.chooseFolder() {
                                model.update { $0.outputDir = url.path }
                            }
                        }
                    }
                }
            }
            LabeledContent("Last recording") {
                Button("Show in Finder") {
                    if let dir = model.lastRecordingDir { Platform.reveal(dir) }
                }
                .disabled(model.lastRecordingDir == nil)
            }
            Toggle(
                "Show cursor in recordings",
                isOn: Binding(
                    get: { model.settings.includeCursor },
                    set: { value in model.update { $0.includeCursor = value } }
                ))
            Toggle(
                "Hide wrec while recording",
                isOn: Binding(
                    get: { model.settings.hideWrec },
                    set: { value in model.update { $0.hideWrec = value } }
                ))
            Toggle(
                "Nerd stats",
                isOn: Binding(
                    get: { model.showNerdLogs },
                    set: { model.setShowNerdLogs($0) }
                ))
        }
        .formStyle(.grouped)
    }
}

// MARK: - Permissions

private struct PermissionsTab: View {
    let model: RecorderModel

    var body: some View {
        Form {
            LabeledContent("Screen Recording") {
                if model.screenPermission.isGranted {
                    Text("Granted").foregroundStyle(.secondary)
                } else {
                    Button("Grant") {
                        Task { await model.refreshScreenPermission(requestIfNeeded: true) }
                    }
                }
            }
            LabeledContent("Microphone") {
                if model.micPermission.isGranted {
                    Text("Granted").foregroundStyle(.secondary)
                } else {
                    Button("Grant") {
                        Task { await model.requestMicPermission() }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .task {
            await model.refreshMicPermission()
            await model.refreshScreenPermission(requestIfNeeded: false)
        }
    }
}

// MARK: - CLI

private struct CliTab: View {
    @State private var cliStatus = Platform.cliInstallStatus()
    @State private var skillStatus = Platform.skillInstallStatus()
    @State private var copied = false

    var body: some View {
        Form {
            LabeledContent("CLI") {
                if cliStatus == .installed {
                    Text("Installed").foregroundStyle(.secondary)
                } else {
                    Button(copied ? "Copied" : "Copy install command") {
                        Platform.copyToClipboard(Platform.cliInstallCommand())
                        copied = true
                    }
                }
            }
            LabeledContent("Claude Code skill") {
                if skillStatus == .installed {
                    Text("Installed").foregroundStyle(.secondary)
                } else {
                    Button(skillStatus.label) {
                        try? Platform.installSkill()
                        skillStatus = Platform.skillInstallStatus()
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            cliStatus = Platform.cliInstallStatus()
            skillStatus = Platform.skillInstallStatus()
        }
    }
}

// MARK: - About / updates

@MainActor
@Observable
private final class UpdateModel {
    private(set) var state: UpdateState
    @ObservationIgnored private var pending: Updater.Release?
    @ObservationIgnored private let daemon = DaemonClient()

    init() {
        state = Updater.eligibleBundle() == nil ? .ineligible : .idle
    }

    func act() {
        if case .available = state { install() } else { check() }
    }

    private func check() {
        state = .checking
        Task {
            do {
                if let release = try await Updater.check() {
                    pending = release
                    state = .available(release.version)
                } else {
                    state = .upToDate
                }
            } catch {
                state = .failed("\(error)")
            }
        }
    }

    private func install() {
        guard let release = pending else { return }
        state = .updating(release.version)
        Task {
            do {
                let ready = try await Updater.downloadAndApply(release, daemon: daemon)
                // Settle config on disk before the relaunch script starts its
                // one-second clock, so the new app never loads a stale file.
                ConfigStore.flush()
                Updater.relaunchAndCleanup(ready)
                NSApp.terminate(nil)
            } catch {
                state = .failed("\(error)")
            }
        }
    }
}

private struct AboutTab: View {
    @State private var updates = UpdateModel()

    var body: some View {
        Form {
            LabeledContent("Version") {
                Text(Bundle.main.shortVersion.isEmpty ? "dev" : Bundle.main.shortVersion)
                    .font(.pixel(12))
                    .foregroundStyle(.secondary)
            }
            LabeledContent("Updates") {
                Button(updates.state.buttonLabel) { updates.act() }
                    .disabled(updates.state.buttonDisabled)
            }
            if case .failed(let message) = updates.state {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            LabeledContent("Source") {
                Button("GitHub") { Platform.open(Platform.githubURL) }
            }
        }
        .formStyle(.grouped)
    }
}
