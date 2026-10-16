// The small Darwin probes the sampler and the report read:
// `phys_footprint`, main-thread CPU time, and the sysctl strings that
// name the machine. macOS-only.
//
// Adapted from L-K-M/Sweetbriar (SystemProbes.swift): same probes, plus
// the off-main-thread peak-footprint sampler it uses so a peak inside a
// main-thread stall still counts.

import Foundation
import Synchronization

enum SystemProbes {
    /// Main-thread CPU time in nanoseconds — correct only on the main
    /// thread.
    static func threadCPUNanoseconds() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)
    }

    /// The process's `phys_footprint`.
    static func physicalFootprint() -> UInt64 {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size
        )
        let result = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(
                to: integer_t.self,
                capacity: Int(count)
            ) { rebound in
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), rebound, &count)
            }
        }
        return result == KERN_SUCCESS ? info.phys_footprint : 0
    }

    /// A sysctl string — `hw.model`, `machdep.cpu.brand_string`.
    static func sysctlString(_ name: String) -> String {
        var size = 0
        sysctlbyname(name, nil, &size, nil, 0)
        guard size > 0 else { return "" }
        var buffer = [CChar](repeating: 0, count: size)
        sysctlbyname(name, &buffer, &size, nil, 0)
        let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    /// A sysctl `int` — `kern.hv_vmm_present` (nonzero: virtualized).
    static func sysctlInt(_ name: String) -> Int32 {
        var value: Int32 = 0
        var size = MemoryLayout<Int32>.size
        sysctlbyname(name, &value, &size, nil, 0)
        return value
    }
}

/// The process's peak footprint over a run, sampled every few
/// milliseconds off the main thread: a peak inside a main-thread stall,
/// which no display tick sees, still counts. `@unchecked Sendable`:
/// the peak and the timer's state are behind their mutexes.
final class FootprintSampler: @unchecked Sendable {
    private static let interval = DispatchTimeInterval.milliseconds(5)

    /// Where the timer is in its life: a dispatch source must be
    /// resumed before it is cancelled and released.
    private enum State {
        case idle
        case running
        case stopped
    }

    private let peak = Mutex<UInt64>(0)
    private let state = Mutex(State.idle)
    private let timer = DispatchSource.makeTimerSource(
        queue: .global(qos: .userInitiated)
    )

    init() {
        timer.schedule(deadline: .now(), repeating: Self.interval)
        timer.setEventHandler { [weak self] in
            self?.sample()
        }
    }

    /// Begin sampling.
    func start() {
        state.withLock { state in
            guard state == .idle else { return }
            state = .running
            timer.resume()
        }
    }

    /// Stop sampling and answer the peak seen.
    func stop() -> UInt64 {
        finish()
        return peak.withLock { $0 }
    }

    /// A sampler dropped mid-run stops too.
    deinit {
        finish()
    }

    /// Cancel the timer once, resuming it first if it never ran.
    private func finish() {
        state.withLock { state in
            if state == .idle {
                timer.resume()
            }
            if state != .stopped {
                timer.cancel()
            }
            state = .stopped
        }
    }

    private func sample() {
        let now = SystemProbes.physicalFootprint()
        peak.withLock { $0 = max($0, now) }
    }
}
