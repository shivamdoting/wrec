// config.json persistence, wire-compatible with the Rust `config` crate
// (snake_case keys, pretty-printed). Writes share one utility queue so disk I/O
// stays off the main actor and rapid user actions still reach disk in order.

import Dispatch
import Foundation

enum ConfigStore {
    private static let saveQueue = DispatchQueue(label: "app.wrec.config-store", qos: .utility)

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return encoder
    }()

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    static func load() -> AppConfig {
        load(currentPath: WrecPaths.configPath(), legacyPaths: legacyConfigPaths())
    }

    static func load(currentPath: URL, legacyPaths: [URL]) -> AppConfig {
        var config: AppConfig
        if let data = try? Data(contentsOf: currentPath),
            let current = try? decoder.decode(AppConfig.self, from: data)
        {
            config = current
        } else if !FileManager.default.fileExists(atPath: currentPath.path),
            let legacy = loadLegacyConfig(currentPath: currentPath, legacyPaths: legacyPaths)
        {
            config = legacy
        } else {
            config = defaults()
        }
        config.settings.applyPresetLimits()
        return config
    }

    static func save(_ config: AppConfig) {
        save(config, to: WrecPaths.configPath())
    }

    static func save(_ config: AppConfig, to path: URL) {
        saveQueue.async { _ = write(config, to: path) }
    }

    /// Wait for queued writes only when the process is about to terminate.
    static func flush() {
        saveQueue.sync {}
    }

    private static func write(_ config: AppConfig, to path: URL) -> Bool {
        do {
            let dir = path.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let data = try encoder.encode(config)
            try data.write(to: path, options: .atomic)
            return true
        } catch {
            NSLog("wrec: config save failed: \(error)")
            return false
        }
    }

    private static func loadLegacyConfig(currentPath: URL, legacyPaths: [URL]) -> AppConfig? {
        for legacyPath in legacyPaths {
            guard let data = try? Data(contentsOf: legacyPath),
                let config = try? decoder.decode(AppConfig.self, from: data)
            else { continue }

            if write(config, to: currentPath) {
                do {
                    try FileManager.default.removeItem(at: legacyPath)
                } catch {
                    NSLog("wrec: migrated config but could not remove \(legacyPath.path): \(error)")
                }
            }
            return config
        }
        return nil
    }

    private static func legacyConfigPaths() -> [URL] {
        let home = FileManager.default.homeDirectoryForCurrentUser
        var paths: [URL] = []
        if let runtimeName = Platform.currentAppBundle()?
            .deletingPathExtension().lastPathComponent,
            runtimeName != "Wrec", runtimeName != "Wrec Dev"
        {
            let support = FileManager.default.urls(
                for: .applicationSupportDirectory, in: .userDomainMask)[0]
            paths.append(support.appending(path: runtimeName).appending(path: "config.json"))
        }
        paths.append(home.appending(path: ".wrec/config.json"))
        paths.append(home.appending(path: ".config/wrec/config.json"))
        paths.append(home.appending(path: ".config/wrec.json"))
        return paths
    }

    private static func defaults() -> AppConfig {
        AppConfig(
            settings: .defaults(),
            selectedTargetKey: nil,
            showNerdLogs: false
        )
    }
}
