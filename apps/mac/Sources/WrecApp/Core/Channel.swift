import Foundation

enum WrecChannel: String, CaseIterable, Codable, Sendable {
    case dev, nightly, release

    static var current: Self {
        let environment = ProcessInfo.processInfo.environment["WREC_CHANNEL"]
        let bundled = Bundle.main.object(forInfoDictionaryKey: "WrecChannel") as? String
        return Self(rawValue: environment ?? bundled ?? "") ?? {
            #if DEBUG
            .dev
            #else
            .release
            #endif
        }()
    }

    var appName: String {
        switch self {
        case .dev: "Wrec Dev"
        case .nightly: "Wrec Nightly"
        case .release: "Wrec"
        }
    }

    var homeName: String {
        switch self {
        case .dev: ".wrec-dev"
        case .nightly: ".wrec-nightly"
        case .release: ".wrec"
        }
    }

    var cliName: String {
        switch self {
        case .dev: "wrec-dev"
        case .nightly: "wrec-nightly"
        case .release: "wrec"
        }
    }

    var runtimeName: String { cliName }

    var displayName: String {
        switch self {
        case .dev: "Dev"
        case .nightly: "Nightly"
        case .release: "Release"
        }
    }

    var badge: String {
        switch self {
        case .dev: "DEV"
        case .nightly: "NIGHTLY"
        case .release: ""
        }
    }

    var jobChangedNotification: String {
        "app.wrec.\(rawValue).job-changed"
    }
}

enum WrecBuild {
    static var gitSHA: String {
        (Bundle.main.object(forInfoDictionaryKey: "WrecGitSHA") as? String) ?? "local"
    }

    static var artifactVersion: String {
        (Bundle.main.object(forInfoDictionaryKey: "WrecArtifactVersion") as? String)
            ?? Bundle.main.shortVersion
    }
}
