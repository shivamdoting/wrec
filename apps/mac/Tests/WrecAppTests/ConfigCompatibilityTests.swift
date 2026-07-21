import Foundation
import Testing
@testable import wrec_app

@Suite(.serialized)
struct ConfigCompatibilityTests {
    @Test
    func legacyEnumAliasesDecodeAndCurrentValuesEncode() throws {
        try assertAlias(CaptureSourceKind.self, aliases: ["Display": "display", "Window": "window"])
        try assertAlias(Codec.self, aliases: ["Hevc": "hevc", "H264": "h264"])
        try assertAlias(FrameRate.self, aliases: ["Fps30": "30", "Fps60": "60"])
        try assertAlias(
            Quality.self,
            aliases: ["Efficient": "efficient", "Balanced": "balanced", "High": "high"]
        )
        try assertAlias(
            Resolution.self,
            aliases: [
                "Native": "native", "R720p": "720p", "R1080p": "1080p", "R2k": "2k",
                "R4k": "4k",
            ]
        )
    }

    @Test
    func missingCurrentConfigMigratesTheFirstValidLegacyConfig() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let current = root.appending(path: "current/config.json")
        let malformed = root.appending(path: "malformed.json")
        let legacy = root.appending(path: "legacy.json")
        try Data("not json".utf8).write(to: malformed)
        try legacyJSON().write(to: legacy)

        let loaded = ConfigStore.load(
            currentPath: current,
            legacyPaths: [malformed, legacy]
        )

        #expect(loaded.settings.source == .window)
        #expect(loaded.settings.codec == .h264)
        #expect(loaded.settings.fps == .fps60)
        #expect(loaded.settings.quality == .high)
        #expect(loaded.settings.resolution == .r4k)
        #expect(loaded.selectedTargetKey == "window:42")
        #expect(loaded.showNerdLogs)
        #expect(FileManager.default.fileExists(atPath: current.path))
        #expect(FileManager.default.fileExists(atPath: malformed.path))
        #expect(!FileManager.default.fileExists(atPath: legacy.path))

        let saved = try Data(contentsOf: current)
        let json = try #require(JSONSerialization.jsonObject(with: saved) as? [String: Any])
        let settings = try #require(json["settings"] as? [String: Any])
        #expect(settings["source"] as? String == "window")
        #expect(settings["codec"] as? String == "h264")
        #expect(settings["fps"] as? String == "60")
        #expect(settings["quality"] as? String == "high")
        #expect(settings["resolution"] as? String == "4k")
    }

    @Test
    func malformedCurrentConfigDoesNotOverwriteFromLegacy() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let current = root.appending(path: "config.json")
        let legacy = root.appending(path: "legacy.json")
        try Data("not json".utf8).write(to: current)
        try legacyJSON().write(to: legacy)

        let loaded = ConfigStore.load(currentPath: current, legacyPaths: [legacy])

        #expect(loaded.settings.source == .display)
        #expect(loaded.selectedTargetKey == nil)
        #expect(try Data(contentsOf: current) == Data("not json".utf8))
        #expect(FileManager.default.fileExists(atPath: legacy.path))
    }

    @Test
    func sequentialSavesLeaveTheLatestConfigOnDisk() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let current = root.appending(path: "config.json")
        var config = AppConfig(
            settings: .defaults(), selectedTargetKey: nil, showNerdLogs: false)

        for quality in Quality.allCases {
            config.settings.quality = quality
            ConfigStore.save(config, to: current)
        }
        ConfigStore.flush()

        let loaded = ConfigStore.load(currentPath: current, legacyPaths: [])
        #expect(loaded.settings.quality == Quality.allCases.last)
    }

    @Test
    func updaterAcceptsAValidBundleWithTheSameIdentifier() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let current = try signedAppBundle(in: root, name: "Current", identifier: "app.wrec.test")
        let replacement = try signedAppBundle(
            in: root, name: "Replacement", identifier: "app.wrec.test")

        try Updater.verifyReplacementBundle(replacement, replacing: current)
    }

    @Test
    func updaterRejectsATamperedOrDifferentlyIdentifiedBundle() throws {
        let root = try temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: root) }
        let current = try signedAppBundle(in: root, name: "Current", identifier: "app.wrec.test")
        let different = try signedAppBundle(
            in: root, name: "Different", identifier: "app.other.test")
        #expect(throws: UpdaterError.self) {
            try Updater.verifyReplacementBundle(different, replacing: current)
        }

        let tampered = try signedAppBundle(
            in: root, name: "Tampered", identifier: "app.wrec.test")
        try Data("#!/bin/sh\necho tampered\n".utf8).write(
            to: tampered.appending(path: "Contents/MacOS/wrec-app"))
        #expect(throws: UpdaterError.self) {
            try Updater.verifyReplacementBundle(tampered, replacing: current)
        }
    }

    private func assertAlias<Value>(
        _ type: Value.Type,
        aliases: [String: String]
    ) throws where Value: Codable & Equatable {
        let decoder = JSONDecoder()
        let encoder = JSONEncoder()
        for (legacy, current) in aliases {
            let value = try decoder.decode(Value.self, from: Data("\"\(legacy)\"".utf8))
            #expect(String(decoding: try encoder.encode(value), as: UTF8.self) == "\"\(current)\"")
        }
    }

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appending(
            path: "wrec-config-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        return directory
    }

    private func signedAppBundle(
        in directory: URL,
        name: String,
        identifier: String
    ) throws -> URL {
        let bundle = directory.appending(path: "\(name).app", directoryHint: .isDirectory)
        let macOS = bundle.appending(path: "Contents/MacOS", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: macOS, withIntermediateDirectories: true)
        let executable = macOS.appending(path: "wrec-app")
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: executable)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755], ofItemAtPath: executable.path)
        let info: [String: Any] = [
            "CFBundleExecutable": "wrec-app",
            "CFBundleIdentifier": identifier,
            "CFBundlePackageType": "APPL",
        ]
        let plist = try PropertyListSerialization.data(
            fromPropertyList: info, format: .xml, options: 0)
        try plist.write(to: bundle.appending(path: "Contents/Info.plist"))

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
        process.arguments = ["--force", "--sign", "-", bundle.path]
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw UpdaterError.message("could not sign updater test bundle")
        }
        return bundle
    }

    private func legacyJSON() -> Data {
        Data(
            #"{"settings":{"source":"Window","codec":"H264","fps":"Fps60","quality":"High","resolution":"R4k","include_system_audio":true,"include_microphone":false,"include_cursor":true,"hide_wrec":false,"output_dir":"/tmp"},"selected_target_key":"window:42","show_nerd_logs":true}"#.utf8
        )
    }
}
