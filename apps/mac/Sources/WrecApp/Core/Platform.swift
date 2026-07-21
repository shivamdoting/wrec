// CLI install status, skill install, folder picking, clipboard, reveal.
// Ports of `crates/app/src/platform.rs`, using native AppKit APIs instead of
// subprocess shell-outs wherever possible (NSWorkspace over `open`,
// NSOpenPanel over osascript).

import AppKit
import Foundation
import UniformTypeIdentifiers

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
    private static let managedMarker = "# managed by wrec"
    private static let installedBin = "/usr/local/bin/wrec"
    private static let installedLib = "/usr/local/lib/wrec"

    // MARK: - CLI

    static func cliInstallStatus() -> CliInstallStatus {
        let fm = FileManager.default
        guard fm.fileExists(atPath: installedBin) else { return .notInstalled }
        guard let wrapper = try? String(contentsOfFile: installedBin, encoding: .utf8),
            wrapper.contains(managedMarker)
        else {
            return .conflict
        }
        let complete = ["wrec", "daemon", "capture-engine"].allSatisfy {
            fm.fileExists(atPath: "\(installedLib)/\($0)")
        }
        return complete ? .installed : .needsUpdate
    }

    static func cliInstallCommand() -> String {
        let version = Bundle.main.shortVersion
        let prefix = version.isEmpty || isDevBundle() ? "" : "WREC_VERSION=\(version) "
        return "curl -fsSL https://wrec.app/install | \(prefix)sh"
    }

    // MARK: - Skill

    private static var skillPath: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".claude/skills/wrec/SKILL.md")
    }

    private static func bundledSkill() -> String? {
        guard let url = Bundle.module.url(forResource: "SKILL", withExtension: "md") else {
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

    static func copyToClipboard(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    // MARK: - Bundle identity

    static func currentAppBundle() -> URL? {
        var url = Bundle.main.bundleURL
        while url.pathExtension != "app" {
            let parent = url.deletingLastPathComponent()
            if parent == url { return nil }
            url = parent
        }
        return url
    }

    static func isDevBundle() -> Bool {
        guard let bundle = currentAppBundle() else { return true }
        if bundle.lastPathComponent.contains("Dev") { return true }
        return (Bundle.main.bundleIdentifier ?? "").contains("wrec.dev")
    }
}

extension Bundle {
    var shortVersion: String {
        (infoDictionary?["CFBundleShortVersionString"] as? String) ?? ""
    }
}
