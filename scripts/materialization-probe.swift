import Darwin
import Dispatch
import Foundation
import Synchronization

private struct PolicySample: Codable, Sendable {
    let process: Int32
    let thread: Int32
    let isMainThread: Bool
}

private func sample() -> PolicySample {
    PolicySample(
        process: getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS),
        thread: getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD),
        isMainThread: Thread.isMainThread
    )
}

private struct ResourceLimitSample: Codable, Sendable {
    let soft: UInt64
    let hard: UInt64
}

private func resourceLimit() throws -> ResourceLimitSample {
    var limit = rlimit()
    guard getrlimit(RLIMIT_NOFILE, &limit) == 0 else {
        throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
    return ResourceLimitSample(soft: UInt64(limit.rlim_cur), hard: UInt64(limit.rlim_max))
}

private struct DescriptorPressureSample: Codable, Sendable {
    let requested: Int
    let held: Int
    let failures: [String]
}

// Hold `requested` simultaneous read-only /dev/null descriptors with O_CLOEXEC,
// close every descriptor, and report each open failure explicitly.
private func pressureDescriptors(requested: Int = 1024) -> DescriptorPressureSample {
    var descriptors: [Int32] = []
    descriptors.reserveCapacity(requested)
    var failures: [String] = []
    for attempt in 0..<requested {
        let descriptor = open("/dev/null", O_RDONLY | O_CLOEXEC)
        if descriptor >= 0 {
            descriptors.append(descriptor)
        } else {
            let reason = String(cString: strerror(errno))
            failures.append("open /dev/null attempt \(attempt): \(reason) (errno \(errno))")
        }
    }
    for descriptor in descriptors {
        if close(descriptor) != 0 {
            let code = errno
            failures.append("close descriptor \(descriptor): \(String(cString: strerror(code))) (errno \(code))")
        }
    }
    return DescriptorPressureSample(requested: requested, held: descriptors.count, failures: failures)
}

private struct ChildProbe: Codable, Sendable {
    let process: Int32
    let thread: Int32
    let isMainThread: Bool
    let limit: ResourceLimitSample
    let descriptors: DescriptorPressureSample
}

private struct PolicyReport: Codable {
    let off: Int32
    let on: Int32
    let systemDefault: Int32
    let samples: [PolicySample]
    let child: ChildProbe
    let limit: ResourceLimitSample
    let descriptors: DescriptorPressureSample
}

// Spawn the child with posix_spawn so the inherited RLIMIT_NOFILE survives:
// Foundation's Process/Pipe and Grand Central Dispatch both raise the soft
// limit to 2560, which would hide the launchd-provided budget.
private func spawnChild() throws -> ChildProbe {
    var fds: [Int32] = [0, 0]
    guard pipe(&fds) == 0 else {
        fatalError("pipe failed: \(String(cString: strerror(errno)))")
    }
    var fileActions: posix_spawn_file_actions_t? = nil
    posix_spawn_file_actions_init(&fileActions)
    posix_spawn_file_actions_adddup2(&fileActions, fds[1], STDOUT_FILENO)
    posix_spawn_file_actions_addclose(&fileActions, fds[0])
    let executable = CommandLine.arguments[0]
    var arguments: [UnsafeMutablePointer<CChar>?] = [strdup(executable), strdup("--child"), nil]
    var pid: pid_t = 0
    let spawnResult = posix_spawn(&pid, executable, &fileActions, nil, &arguments, environ)
    posix_spawn_file_actions_destroy(&fileActions)
    arguments.forEach { free($0) }
    close(fds[1])
    guard spawnResult == 0 else {
        close(fds[0])
        fatalError("posix_spawn failed: \(String(cString: strerror(spawnResult)))")
    }
    var data = Data()
    let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: 4096)
    defer { buffer.deallocate() }
    while true {
        let count = read(fds[0], buffer, 4096)
        if count <= 0 { break }
        data.append(buffer, count: Int(count))
    }
    close(fds[0])
    var status: Int32 = 0
    waitpid(pid, &status, 0)
    guard status & 0x7f == 0, (status >> 8) & 0xff == Int32(EXIT_SUCCESS) else {
        fatalError("Child policy probe failed")
    }
    return try JSONDecoder().decode(ChildProbe.self, from: data)
}

// Inspect main/worker threads and inherited child policy without opening data.
private func makeReport() throws -> PolicyReport {
    // Capture the inherited budget and pressure it before GCD or Foundation's
    // Process/Pipe raise the soft limit.
    let inheritedLimit = try resourceLimit()
    let inheritedPressure = pressureDescriptors()
    let child = try spawnChild()
    let samples = Mutex([sample()])
    let workerCount = 4
    let completed = DispatchGroup()
    for _ in 0..<workerCount {
        completed.enter()
        Thread.detachNewThread {
            let observed = sample()
            samples.withLock { $0.append(observed) }
            completed.leave()
        }
    }
    completed.wait()
    return PolicyReport(
        off: IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        on: IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        systemDefault: IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
        samples: samples.withLock { $0 },
        child: child,
        limit: inheritedLimit,
        descriptors: inheritedPressure
    )
}

if CommandLine.arguments.dropFirst() == ["--child"] {
    let limit = try resourceLimit()
    let descriptors = pressureDescriptors()
    let observed = sample()
    let payload = ChildProbe(
        process: observed.process,
        thread: observed.thread,
        isMainThread: observed.isMainThread,
        limit: limit,
        descriptors: descriptors
    )
    FileHandle.standardOutput.write(try JSONEncoder().encode(payload))
    exit(EXIT_SUCCESS)
}
guard CommandLine.arguments.count == 2 else {
    fatalError("Expected an absolute report path")
}
try JSONEncoder().encode(makeReport()).write(
    to: URL(fileURLWithPath: CommandLine.arguments[1]), options: .atomic
)
