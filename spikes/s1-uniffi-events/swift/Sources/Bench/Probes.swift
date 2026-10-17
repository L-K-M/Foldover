// The small Darwin probes the report's environment and memory
// readings use: `phys_footprint` and the sysctl values that name the
// machine.
//
// Adapted from L-K-M/Sweetbriar.

import Foundation

enum Probes {
    /// Scan this still-live process after stream teardown, preserving failures verbatim.
    static func scanHeap() -> HeapScan {
        let process = Process()
        let output = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/leaks")
        process.arguments = ["\(ProcessInfo.processInfo.processIdentifier)"]
        process.standardOutput = output
        process.standardError = output

        do {
            try process.run()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return HeapScan(
                exitStatus: process.terminationStatus,
                output: String(decoding: data, as: UTF8.self)
            )
        } catch {
            return HeapScan(exitStatus: nil, output: "\(error)")
        }
    }

    /// The process's `phys_footprint` — what the memory check reads
    /// before and after the repeated streams.
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
                task_info(
                    mach_task_self_, task_flavor_t(TASK_VM_INFO),
                    rebound, &count
                )
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

    /// A sysctl `int` — `kern.hv_vmm_present` (nonzero means the
    /// runner is virtualized).
    static func sysctlInt(_ name: String) -> Int32 {
        var value: Int32 = 0
        var size = MemoryLayout<Int32>.size
        sysctlbyname(name, &value, &size, nil, 0)
        return value
    }
}
