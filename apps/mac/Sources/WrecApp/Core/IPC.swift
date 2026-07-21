// The daemon client: newline-delimited JSON over a Unix domain socket,
// one request per connection, protocol version 1. A faithful, allocation-lean
// port of `crates/control/src/client.rs`.
//
// Performance notes:
// - Raw POSIX sockets with kernel SO_RCVTIMEO/SO_SNDTIMEO — no Foundation
//   stream machinery, no run-loop scheduling, no dispatch sources. A full
//   round trip is: socket, connect, one write, a few reads, close.
// - JSONEncoder/Decoder instances are created once and reused.
// - The actor serializes requests (the daemon is single-coordinator anyway)
//   and keeps every syscall off the main thread by construction.

import Darwin
import Dispatch
import Foundation

enum IPCError: Error, CustomStringConvertible {
    case unreachable(String)
    case daemon(AgentError)
    case protocolMismatch(Int)
    case malformed(String)

    var description: String {
        switch self {
        case .unreachable(let why): "daemon unreachable: \(why)"
        case .daemon(let err): err.message
        case .protocolMismatch(let got): "daemon protocol \(got) ≠ expected \(Self.protocolVersion)"
        case .malformed(let why): "malformed daemon response: \(why)"
        }
    }

    static let protocolVersion = 1
}

actor DaemonClient {
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder
    private let ioQueue = DispatchQueue(label: "dev.wrec.daemon-client", qos: .userInitiated)
    private let timeoutSeconds: Int
    private var ensureTask: Task<Void, Error>?

    init(timeoutSeconds: Int = 10) {
        encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.timeoutSeconds = timeoutSeconds
    }

    // MARK: - Public API (mirrors control::DaemonClient)

    /// Every ensure re-validates the protocol version, matching the Rust
    /// client — the daemon can be swapped underneath us by an update.
    func ensure() async throws {
        if let ensureTask {
            try await ensureTask.value
            return
        }

        let task = Task { try await performEnsure() }
        ensureTask = task
        defer { ensureTask = nil }
        try await task.value
    }

    private func performEnsure() async throws {
        if let status = try? await status() {
            try checkProtocol(status)
            return
        }
        try spawnDaemon()
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if let status = try? await status() {
                try checkProtocol(status)
                return
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        throw IPCError.unreachable("daemon did not come up within 10s")
    }

    func status() async throws -> DaemonStatus {
        try await request("daemon.status", EmptyParams())
    }

    func stopDaemon() async throws {
        struct StopResult: Decodable { let stopping: Bool }
        let _: StopResult = try await request("daemon.stop", EmptyParams())
    }

    func screenPermissionStatus() async throws -> PermissionStatus {
        let result: PermissionResult = try await request("permission.status", EmptyParams())
        return result.status
    }

    func requestScreenPermission() async throws -> PermissionStatus {
        let result: PermissionResult = try await request("permission.request", EmptyParams())
        return result.status
    }

    func listTargets() async throws -> [CaptureTarget] {
        let result: TargetsResult = try await request("targets.list", EmptyParams())
        return result.targets
    }

    func startRecording(_ params: StartRecordingParams) async throws -> JobSnapshot {
        let result: JobResult = try await request("record.start", params)
        return result.job
    }

    func showJob(_ id: UInt64) async throws -> JobSnapshot {
        let result: JobResult = try await request("job.show", JobIdParams(jobId: id))
        return result.job
    }

    func pauseJob(_ id: UInt64) async throws -> JobSnapshot {
        let result: JobResult = try await request("job.pause", JobIdParams(jobId: id))
        return result.job
    }

    func resumeJob(_ id: UInt64) async throws -> JobSnapshot {
        let result: JobResult = try await request("job.resume", JobIdParams(jobId: id))
        return result.job
    }

    func stopJob(_ id: UInt64) async throws -> JobSnapshot {
        let result: JobResult = try await request("job.stop", JobIdParams(jobId: id))
        return result.job
    }

    // MARK: - Round trip

    private struct RequestEnvelope<P: Encodable>: Encodable {
        let id: UInt64
        let method: String
        let params: P
    }

    private struct ResponseEnvelope<R: Decodable>: Decodable {
        let ok: Bool
        let result: R?
        let error: AgentError?
    }

    private func request<P: Encodable, R: Decodable>(
        _ method: String,
        _ params: P
    ) async throws -> R {
        let id = UInt64(Date().timeIntervalSince1970 * 1000)
        var encodedPayload = try encoder.encode(
            RequestEnvelope(id: id, method: method, params: params)
        )
        encodedPayload.append(0x0A)
        let payload = encodedPayload

        let line = try await withCheckedThrowingContinuation { continuation in
            ioQueue.async { [timeoutSeconds] in
                continuation.resume(
                    with: Result { try Self.roundTrip(payload, timeoutSeconds: timeoutSeconds) })
            }
        }
        let envelope = try decode(ResponseEnvelope<R>.self, from: line)
        if envelope.ok, let result = envelope.result { return result }
        if let error = envelope.error { throw IPCError.daemon(error) }
        throw IPCError.malformed("ok response without result for \(method)")
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do {
            return try decoder.decode(type, from: data)
        } catch {
            throw IPCError.malformed("\(error)")
        }
    }

    /// One connection, one line out, one line back.
    private static func roundTrip(_ payload: Data, timeoutSeconds: Int) throws -> Data {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw IPCError.unreachable("socket(): \(errnoString())") }
        defer { close(fd) }

        var timeout = timeval(tv_sec: timeoutSeconds, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let path = WrecPaths.socketPath().path
        let ok: Bool = path.withCString { cstr in
            withUnsafeMutableBytes(of: &addr.sun_path) { dest in
                let len = strlen(cstr)
                guard len < dest.count else { return false }
                memcpy(dest.baseAddress!, cstr, len + 1)
                return true
            }
        }
        guard ok else { throw IPCError.unreachable("socket path too long") }

        var connected = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(fd, sa, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if connected < 0, errno == EINTR {
            // A signal interrupted the wait, but the attempt continues in the
            // kernel — a second connect() would fail with EISCONN. Per
            // connect(2), wait for writability and read the outcome instead.
            var pollFd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
            // One deadline spans every retry, so a stream of signals cannot
            // stretch the wait past the configured timeout.
            let deadline = DispatchTime.now() + .seconds(timeoutSeconds)
            var ready: Int32
            repeat {
                let now = DispatchTime.now()
                let remainingMs =
                    now >= deadline
                    ? 0
                    : Int32(clamping: (deadline.uptimeNanoseconds - now.uptimeNanoseconds) / 1_000_000)
                ready = poll(&pollFd, 1, remainingMs)
            } while ready < 0 && errno == EINTR
            guard ready >= 0 else { throw IPCError.unreachable("poll(): \(errnoString())") }
            guard ready > 0 else { throw IPCError.unreachable("connect(): timed out") }
            var soError: Int32 = 0
            var soErrorLen = socklen_t(MemoryLayout<Int32>.size)
            guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &soError, &soErrorLen) == 0 else {
                throw IPCError.unreachable("getsockopt(SO_ERROR): \(errnoString())")
            }
            guard soError == 0 else {
                throw IPCError.unreachable("connect(): \(String(cString: strerror(soError)))")
            }
            connected = 0
        }
        guard connected == 0 else { throw IPCError.unreachable("connect(): \(errnoString())") }

        var written = 0
        try payload.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) in
            while written < bytes.count {
                let n = write(fd, bytes.baseAddress! + written, bytes.count - written)
                if n < 0, errno == EINTR { continue }
                guard n > 0 else { throw IPCError.unreachable("write(): \(errnoString())") }
                written += n
            }
        }

        var line = Data(capacity: 4096)
        var buffer = [UInt8](repeating: 0, count: 16384)
        while true {
            let n = read(fd, &buffer, buffer.count)
            if n < 0, errno == EINTR { continue }
            if n < 0 { throw IPCError.unreachable("read(): \(errnoString())") }
            if n == 0 { break }
            if let newline = buffer[0..<n].firstIndex(of: 0x0A) {
                line.append(contentsOf: buffer[0..<newline])
                break
            }
            line.append(contentsOf: buffer[0..<n])
        }
        guard !line.isEmpty else { throw IPCError.unreachable("empty response") }
        return line
    }

    private func checkProtocol(_ status: DaemonStatus) throws {
        guard status.protocolVersion == IPCError.protocolVersion else {
            throw IPCError.protocolMismatch(status.protocolVersion)
        }
    }

    // MARK: - Daemon spawn (port of control::ensure_daemon)

    private func spawnDaemon() throws {
        let home = WrecPaths.home()
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)

        guard let launch = Self.daemonLaunch() else {
            throw IPCError.unreachable("no daemon binary found (set WREC_DAEMON_BIN)")
        }

        var fileActions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&fileActions)
        defer { posix_spawn_file_actions_destroy(&fileActions) }
        posix_spawn_file_actions_addopen(&fileActions, 0, "/dev/null", O_RDONLY, 0)
        WrecPaths.daemonLogPath().path.withCString { log in
            _ = posix_spawn_file_actions_addopen(&fileActions, 1, log, O_WRONLY | O_APPEND | O_CREAT, 0o644)
            _ = posix_spawn_file_actions_addopen(&fileActions, 2, log, O_WRONLY | O_APPEND | O_CREAT, 0o644)
        }

        // New session so the daemon outlives this app, matching the Rust
        // client's `process_group(0)`.
        var attrs: posix_spawnattr_t?
        posix_spawnattr_init(&attrs)
        defer { posix_spawnattr_destroy(&attrs) }
        posix_spawnattr_setflags(&attrs, Int16(POSIX_SPAWN_SETSID))

        var environment = ProcessInfo.processInfo.environment
        if let engine = launch.captureEnginePath {
            environment["WREC_CAPTURE_ENGINE_PATH"] = engine
        }

        var pid: pid_t = 0
        let argv: [UnsafeMutablePointer<CChar>?] = [strdup(launch.binary), nil]
        let envp: [UnsafeMutablePointer<CChar>?] =
            environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer {
            argv.forEach { free($0) }
            envp.forEach { free($0) }
        }

        let rc = posix_spawn(&pid, launch.binary, &fileActions, &attrs, argv, envp)
        guard rc == 0 else {
            throw IPCError.unreachable("posix_spawn(daemon): \(String(cString: strerror(rc)))")
        }
    }

    private struct DaemonLaunch {
        let binary: String
        let captureEnginePath: String?
    }

    /// Binary resolution order mirrors `control::daemon_candidates`:
    /// `$WREC_DAEMON_BIN` → sibling `daemon` next to this executable →
    /// `/usr/local/lib/wrec/daemon`. A sibling daemon requires a sibling
    /// `capture-engine`, passed via env.
    private static func daemonLaunch() -> DaemonLaunch? {
        let fm = FileManager.default
        if let override = ProcessInfo.processInfo.environment["WREC_DAEMON_BIN"],
            fm.isExecutableFile(atPath: override)
        {
            return DaemonLaunch(binary: override, captureEnginePath: nil)
        }
        if let exe = Bundle.main.executableURL?.resolvingSymlinksInPath() {
            let dir = exe.deletingLastPathComponent()
            let sibling = dir.appending(path: "daemon").path
            let engine = dir.appending(path: "capture-engine").path
            if fm.isExecutableFile(atPath: sibling), fm.isExecutableFile(atPath: engine) {
                return DaemonLaunch(binary: sibling, captureEnginePath: engine)
            }
        }
        // The installed daemon is a release build serving `~/.wrec`; a debug
        // shell polls `~/.wrec-dev` and would never see it come up — it'd
        // just orphan a release daemon and time out.
        #if !DEBUG
        let installed = "/usr/local/lib/wrec/daemon"
        let installedEngine = "/usr/local/lib/wrec/capture-engine"
        if fm.isExecutableFile(atPath: installed), fm.isExecutableFile(atPath: installedEngine) {
            return DaemonLaunch(binary: installed, captureEnginePath: installedEngine)
        }
        #endif
        return nil
    }
}

private func errnoString() -> String {
    String(cString: strerror(errno))
}
