// Self-update: GitHub latest release → verified download → atomic bundle
// swap → relaunch. Port of `crates/app/src/updater.rs` with native APIs:
// URLSession instead of curl, CryptoKit streaming SHA-256 instead of shasum.
// Only `tar` and `ditto` remain as subprocesses (both are the right tool).

import CryptoKit
import Foundation

enum UpdateState: Equatable {
    case idle
    case checking
    case upToDate
    case available(String)
    case updating(String)
    case failed(String)
    case ineligible

    var buttonLabel: String {
        switch self {
        case .idle: "Check for updates"
        case .checking: "Checking…"
        case .upToDate: "Up to date"
        case .available(let version): "Update to \(version)"
        case .updating: "Updating…"
        case .failed: "Check again"
        case .ineligible: "Unavailable"
        }
    }

    var buttonDisabled: Bool {
        switch self {
        case .checking, .upToDate, .updating, .ineligible: true
        default: false
        }
    }
}

struct ReadyUpdate: Sendable {
    let version: String
    let bundle: URL
    let oldBundle: URL
}

enum UpdaterError: Error, CustomStringConvertible {
    case message(String)

    var description: String {
        switch self { case .message(let text): text }
    }
}

enum Updater {
    private static var repo: String {
        ProcessInfo.processInfo.environment["WREC_REPO"] ?? "shivamdoting/wrec"
    }

    static var currentVersion: String { Bundle.main.shortVersion }

    /// Update only a packaged, non-dev bundle — dev and `swift run` builds
    /// update by rebuilding.
    static func eligibleBundle() -> URL? {
        guard let bundle = Platform.currentAppBundle(), !Platform.isDevBundle() else {
            return nil
        }
        return bundle
    }

    struct Release: Sendable {
        let version: String
        let assetURL: URL
        let sha256: String?
    }

    static func check() async throws -> Release? {
        let release = try await latestRelease()
        guard isNewer(release.version, than: currentVersion) else { return nil }
        return release
    }

    static func downloadAndApply(_ release: Release, daemon: DaemonClient) async throws
        -> ReadyUpdate
    {
        guard let bundle = eligibleBundle() else {
            throw UpdaterError.message("this build updates by rebuilding")
        }

        let work = try exclusiveWorkDir()
        defer { try? FileManager.default.removeItem(at: work) }

        let archive = work.appending(path: "update.tar.gz")
        let (downloaded, _) = try await URLSession.shared.download(from: release.assetURL)
        try FileManager.default.moveItem(at: downloaded, to: archive)

        guard let expected = release.sha256 else {
            throw UpdaterError.message("release has no published digest; refusing to update")
        }
        let actual = try sha256(of: archive)
        guard actual == expected.lowercased() else {
            throw UpdaterError.message("digest mismatch: expected \(expected), got \(actual)")
        }

        let extracted = work.appending(path: "extracted")
        try FileManager.default.createDirectory(at: extracted, withIntermediateDirectories: true)
        try run("/usr/bin/tar", "-xzf", archive.path, "-C", extracted.path)
        let newBundle = try findAppBundle(in: extracted)
        try verifyReplacementBundle(newBundle, replacing: bundle)

        // Stop a running daemon so its binaries can be swapped. No daemon is
        // nothing to stop; only an active recording (daemon_busy) aborts.
        do {
            try await daemon.stopDaemon()
        } catch IPCError.unreachable {
            // Already stopped.
        } catch let IPCError.daemon(err) where err.code != "daemon_busy" {
            // Anything short of an active recording won't block the swap.
        }

        let oldBundle = try swapBundles(current: bundle, replacement: newBundle)
        return ReadyUpdate(version: release.version, bundle: bundle, oldBundle: oldBundle)
    }

    /// Detached: relaunch the swapped bundle, then reap the old one.
    static func relaunchAndCleanup(_ update: ReadyUpdate) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = [
            "-c", #"sleep 1; open -n "$0"; sleep 5; rm -rf "$1""#,
            update.bundle.path, update.oldBundle.path,
        ]
        try? process.run()
    }

    // MARK: - GitHub

    private static func latestRelease() async throws -> Release {
        var request = URLRequest(
            url: URL(string: "https://api.github.com/repos/\(repo)/releases/latest")!)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("wrec-app", forHTTPHeaderField: "User-Agent")
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw UpdaterError.message("release check failed")
        }

        struct Asset: Decodable {
            let name: String
            let browserDownloadUrl: URL
            let digest: String?
        }
        struct ReleaseJSON: Decodable {
            let tagName: String
            let assets: [Asset]
        }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let release = try decoder.decode(ReleaseJSON.self, from: data)

        let version = release.tagName.hasPrefix("v") ? String(release.tagName.dropFirst()) : release.tagName
        #if arch(arm64)
        let assetName = "wrec-app-aarch64-apple-darwin.tar.gz"
        #else
        let assetName = "wrec-app-x86_64-apple-darwin.tar.gz"
        #endif
        guard let asset = release.assets.first(where: { $0.name == assetName }) else {
            throw UpdaterError.message("release \(version) has no asset \(assetName)")
        }
        let sha = asset.digest.map { $0.hasPrefix("sha256:") ? String($0.dropFirst(7)) : $0 }
        return Release(version: version, assetURL: asset.browserDownloadUrl, sha256: sha)
    }

    static func isNewer(_ candidate: String, than current: String) -> Bool {
        if let a = parseVersion(candidate), let b = parseVersion(current) {
            for (x, y) in zip(a, b) where x != y { return x > y }
            return false
        }
        return candidate != current
    }

    /// `x.y.z` with trailing junk tolerated per component (`0.3.0-rc1` →
    /// `[0, 3, 0]`), so a suffixed tag equal to or older than the installed
    /// version is never offered as an update on every launch.
    private static func parseVersion(_ raw: String) -> [Int]? {
        let parts = raw.split(separator: ".", maxSplits: 2).map { Int($0.prefix(while: \.isNumber)) }
        guard parts.count == 3 else { return nil }
        let numbers = parts.compactMap { $0 }
        return numbers.count == 3 ? numbers : nil
    }

    // MARK: - Verify / swap

    private static func sha256(of file: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: file)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private static func exclusiveWorkDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appending(path: "wrec-update-\(ProcessInfo.processInfo.processIdentifier)-\(UInt64.random(in: 0...UInt64.max))")
        try FileManager.default.createDirectory(
            at: dir, withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o700])
        return dir
    }

    private static func findAppBundle(in dir: URL) throws -> URL {
        let fm = FileManager.default
        let contents = try fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
        for candidate in contents where candidate.pathExtension == "app" {
            let exe = candidate.appending(path: "Contents/MacOS/wrec-app")
            if fm.fileExists(atPath: exe.path) { return candidate }
        }
        throw UpdaterError.message("archive contains no wrec app bundle")
    }

    /// Reject malformed or differently identified bundles before the daemon
    /// is stopped. Ad-hoc releases fall back to the signing identifier; a
    /// Developer ID-signed app also pins replacements to its Apple team.
    static func verifyReplacementBundle(_ replacement: URL, replacing current: URL) throws {
        try run("/usr/bin/codesign", "--verify", "--deep", "--strict", replacement.path)

        let currentMetadata = try codesignMetadata(for: current)
        let replacementMetadata = try codesignMetadata(for: replacement)
        guard let currentIdentifier = currentMetadata["Identifier"],
            replacementMetadata["Identifier"] == currentIdentifier
        else {
            throw UpdaterError.message("update bundle has a different signing identifier")
        }

        if let currentTeam = teamIdentifier(in: currentMetadata) {
            guard currentTeam.count == 10,
                currentTeam.allSatisfy({ $0.isASCII && ($0.isUppercase || $0.isNumber) })
            else {
                throw UpdaterError.message("installed app has an invalid developer team identifier")
            }
            let requirement =
                "=anchor apple generic and certificate leaf[subject.OU] = \"\(currentTeam)\""
            do {
                try run(
                    "/usr/bin/codesign", "--verify", "--deep", "--strict",
                    "--test-requirement", requirement, replacement.path)
            } catch {
                throw UpdaterError.message("update bundle is not signed by the expected developer team")
            }
        }
    }

    private static func codesignMetadata(for bundle: URL) throws -> [String: String] {
        let process = Process()
        let output = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
        process.arguments = ["-d", "--verbose=4", bundle.path]
        process.standardOutput = output
        process.standardError = output
        try process.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw UpdaterError.message("could not inspect update bundle signature")
        }

        return String(decoding: data, as: UTF8.self)
            .split(whereSeparator: \.isNewline)
            .reduce(into: [:]) { metadata, line in
                let parts = line.split(separator: "=", maxSplits: 1)
                if parts.count == 2 { metadata[String(parts[0])] = String(parts[1]) }
            }
    }

    private static func teamIdentifier(in metadata: [String: String]) -> String? {
        guard let value = metadata["TeamIdentifier"], value != "not set" else { return nil }
        return value
    }

    /// Stage with ditto (preserves signature), then two atomic same-dir
    /// renames. Rolls back if the final rename fails.
    private static func swapBundles(current: URL, replacement: URL) throws -> URL {
        let fm = FileManager.default
        let parent = current.deletingLastPathComponent()
        let name = current.lastPathComponent
        let pid = ProcessInfo.processInfo.processIdentifier
        let staged = parent.appending(path: ".\(name).staged-\(pid)")
        let old = parent.appending(path: ".\(name).old-\(pid)")

        try run("/usr/bin/ditto", replacement.path, staged.path)
        try fm.moveItem(at: current, to: old)
        do {
            try fm.moveItem(at: staged, to: current)
        } catch {
            try? fm.moveItem(at: old, to: current)
            try? fm.removeItem(at: staged)
            throw error
        }
        return old
    }

    private static func run(_ tool: String, _ args: String...) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = args
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw UpdaterError.message(
                "\(URL(fileURLWithPath: tool).lastPathComponent) exited \(process.terminationStatus)")
        }
    }
}
