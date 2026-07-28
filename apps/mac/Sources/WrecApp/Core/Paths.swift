// Path resolution, mirrored from Rust's `wrec-channel` crate. The explicit
// channel keeps contributor, public nightly, and stable state independent.

import Foundation

enum WrecPaths {
    /// `$WREC_HOME` | the active channel's daemon home.
    static func home() -> URL {
        if let override = ProcessInfo.processInfo.environment["WREC_HOME"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appending(path: WrecChannel.current.homeName)
    }

    static func socketPath() -> URL { home().appending(path: "wrec.sock") }

    static func daemonLogPath() -> URL { home().appending(path: "daemon.log") }

    /// `$WREC_DATA_DIR` | the active channel's Application Support directory.
    static func dataDir() -> URL {
        if let override = ProcessInfo.processInfo.environment["WREC_DATA_DIR"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appending(path: WrecChannel.current.appName)
    }

    static func configPath() -> URL { dataDir().appending(path: "config.json") }

    static func defaultOutputDir() -> URL {
        return FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Movies").appending(path: WrecChannel.current.appName)
    }
}
