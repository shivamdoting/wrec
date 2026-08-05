// CLI install status, skill install, folder picking, clipboard, reveal.
// Ports of `crates/app/src/platform.rs`, using native AppKit APIs instead of
// subprocess shell-outs wherever possible (NSWorkspace over `open`,
// NSOpenPanel over osascript).

import AppKit
import Foundation
import UniformTypeIdentifiers

enum WrecResources {
    /// SwiftPM executable targets look for Bundle.module beside the `.app`
    /// root, which is not a code-signable bundle layout. Packaged builds keep
    /// the generated resource bundle in Contents/Resources; source/test builds
    /// fall back to SwiftPM's normal accessor.
    static var bundle: Bundle {
        if let resources = Bundle.main.resourceURL,
            let candidates = try? FileManager.default.contentsOfDirectory(
                at: resources, includingPropertiesForKeys: nil),
            let bundleURL = candidates.first(where: {
                $0.pathExtension == "bundle"
                    && $0.lastPathComponent.hasPrefix("wrec-mac_")
            }),
            let packaged = Bundle(url: bundleURL)
        {
            return packaged
        }
        return Bundle.module
    }
}

enum CliInstallStatus: Equatable {
    case installed
    case needsUpdate
    case notInstalled
    case conflict

    var label: String {
        switch self {
        case .installed: "Installed"
        case .needsUpdate: "Update"
        case .notInstalled: "Copy"
        case .conflict: "Copy"
        }
    }
}

enum SkillInstallStatus: Equatable {
    case installed, needsUpdate, notInstalled

    var label: String {
        switch self {
        case .installed: "Installed"
        case .needsUpdate: "Update"
        case .notInstalled: "Install"
        }
    }
}

enum Platform {
    static let githubURL = URL(string: "https://github.com/shivamdoting/wrec")!
    private static let screenRecordingPrivacyURL = URL(
        string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
    )!
    private static let managedMarker = "# managed by wrec"
    private static var installedBin: String { "/usr/local/bin/\(WrecChannel.current.cliName)" }
    private static var installedLib: String { "/usr/local/lib/\(WrecChannel.current.runtimeName)" }

    // MARK: - CLI

    static func cliInstallStatus() -> CliInstallStatus {
        let fm = FileManager.default
        guard fm.fileExists(atPath: installedBin) else { return .notInstalled }
        guard let wrapper = try? String(contentsOfFile: installedBin, encoding: .utf8),
            wrapper.contains(managedMarker)
        else {
            return .conflict
        }
        let complete = [WrecChannel.current.cliName, "daemon", "capture-engine"].allSatisfy {
            fm.fileExists(atPath: "\(installedLib)/\($0)")
        }
        return complete ? .installed : .needsUpdate
    }

    static func cliInstallCommand() -> String {
        if WrecChannel.current == .dev {
            return "cargo run -p cli -- help"
        }
        let version = Bundle.main.shortVersion
        let versionPrefix =
            version.isEmpty || WrecChannel.current == .nightly ? "" : "WREC_VERSION=\(version) "
        let channelPrefix =
            WrecChannel.current == .nightly ? "WREC_CHANNEL=nightly " : ""
        return "curl -fsSL https://wrec.app/install | \(channelPrefix)\(versionPrefix)sh"
    }

    // MARK: - Skill

    private static var skillPath: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".claude/skills/wrec/SKILL.md")
    }

    private static func bundledSkill() -> String? {
        guard let url = WrecResources.bundle.url(forResource: "SKILL", withExtension: "md") else {
            return nil
        }
        return try? String(contentsOf: url, encoding: .utf8)
    }

    static func skillInstallStatus() -> SkillInstallStatus {
        guard let bundled = bundledSkill() else { return .installed }
        guard let existing = try? String(contentsOf: skillPath, encoding: .utf8) else {
            return .notInstalled
        }
        return existing == bundled ? .installed : .needsUpdate
    }

    static func installSkill() throws {
        guard let bundled = bundledSkill() else { return }
        let dir = skillPath.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try bundled.write(to: skillPath, atomically: true, encoding: .utf8)
    }

    // MARK: - Files / URLs

    @MainActor
    static func chooseFolder() async -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        NSApp.activate(ignoringOtherApps: true)
        return await withCheckedContinuation { continuation in
            panel.begin { response in
                continuation.resume(returning: response == .OK ? panel.url : nil)
            }
        }
    }

    static func reveal(_ url: URL) {
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    static func open(_ url: URL) {
        NSWorkspace.shared.open(url)
    }

    static func openScreenRecordingPrivacySettings() {
        open(screenRecordingPrivacyURL)
    }

    static func copyToClipboard(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    // MARK: - Bundle identity

    static func currentAppBundle() -> URL? {
        appBundle(containing: Bundle.main.bundleURL)
    }

    static func appBundle(containing start: URL) -> URL? {
        var url = start
        while url.pathExtension != "app" {
            // NSURL-bridged URLs never reach a fixed point: deleting the last
            // component of "/" yields "/..", so guard on the root path itself.
            guard url.path != "/" else { return nil }
            let parent = url.deletingLastPathComponent()
            if parent.path == url.path { return nil }
            url = parent
        }
        return url
    }

    static func isDevBundle() -> Bool {
        WrecChannel.current == .dev
    }
}

extension Bundle {
    var shortVersion: String {
        (infoDictionary?["CFBundleShortVersionString"] as? String) ?? ""
    }
}
