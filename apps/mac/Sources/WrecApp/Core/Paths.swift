// Path resolution, mirrored from `crates/control/src/paths.rs` and
// `crates/config/src/lib.rs`. Debug builds use the `-dev` namespaces so a
// development shell never collides with an installed wrec.

import Foundation

enum WrecPaths {
    /// `$WREC_HOME` | `~/.wrec` (release) | `~/.wrec-dev` (debug).
    /// Holds the daemon socket and daemon log.
    static func home() -> URL {
        if let override = ProcessInfo.processInfo.environment["WREC_HOME"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        #if DEBUG
        let name = ".wrec-dev"
        #else
        let name = ".wrec"
        #endif
        return FileManager.default.homeDirectoryForCurrentUser.appending(path: name)
    }

    static func socketPath() -> URL { home().appending(path: "wrec.sock") }

    static func daemonLogPath() -> URL { home().appending(path: "daemon.log") }

    /// `$WREC_DATA_DIR` | `~/Library/Application Support/{Wrec|Wrec Dev}`.
    /// Holds `config.json`, logs, and the recordings database.
    static func dataDir() -> URL {
        if let override = ProcessInfo.processInfo.environment["WREC_DATA_DIR"], !override.isEmpty {
            return URL(fileURLWithPath: override, isDirectory: true)
        }
        #if DEBUG
        let name = "Wrec Dev"
        #else
        let name = "Wrec"
        #endif
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support.appending(path: name)
    }

    static func configPath() -> URL { dataDir().appending(path: "config.json") }

    static func defaultOutputDir() -> URL {
        #if DEBUG
        let name = "Wrec Dev"
        #else
        let name = "Wrec"
        #endif
        return FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Movies").appending(path: name)
    }
}
