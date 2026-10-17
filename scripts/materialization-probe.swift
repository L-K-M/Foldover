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

private struct PolicyReport: Codable {
    let off: Int32
    let on: Int32
    let systemDefault: Int32
    let samples: [PolicySample]
    let child: PolicySample
}

// Inspect main/worker threads and inherited child policy without opening data.
private func makeReport() throws -> PolicyReport {
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
    let pipe = Pipe()
    let child = Process()
    child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
    child.arguments = ["--child"]
    child.standardOutput = pipe
    try child.run()
    let childData = pipe.fileHandleForReading.readDataToEndOfFile()
    child.waitUntilExit()
    guard child.terminationReason == .exit, child.terminationStatus == EXIT_SUCCESS else {
        fatalError("Child policy probe failed")
    }
    return PolicyReport(
        off: IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        on: IOPOL_MATERIALIZE_DATALESS_FILES_ON,
        systemDefault: IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
        samples: samples.withLock { $0 },
        child: try JSONDecoder().decode(PolicySample.self, from: childData)
    )
}

if CommandLine.arguments.dropFirst() == ["--child"] {
    FileHandle.standardOutput.write(try JSONEncoder().encode(sample()))
    exit(EXIT_SUCCESS)
}
guard CommandLine.arguments.count == 2 else {
    fatalError("Expected an absolute report path")
}
try JSONEncoder().encode(makeReport()).write(
    to: URL(fileURLWithPath: CommandLine.arguments[1]), options: .atomic
)
