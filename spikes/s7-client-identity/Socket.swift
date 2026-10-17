import Darwin
import Dispatch
import Foundation

enum ProbeError: Error {
    case invalid(String)
    case system(String, Int32)
    case security(String, OSStatus)
}

// Shared only by the throwaway server and client, not a production transport.
enum ProbeSocket {
    static let timeoutSeconds = 5
    private static let frameLimit = 1_024

    static func address(_ path: String) throws -> sockaddr_un {
        var address = sockaddr_un()
        let bytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard !bytes.contains(0), bytes.count < capacity else {
            throw ProbeError.invalid("Socket path needs fewer than \(capacity) UTF-8 bytes")
        }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        address.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            buffer.copyBytes(from: bytes + [0])
        }
        return address
    }

    static func open() throws -> Int32 {
        let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw ProbeError.system("socket", errno) }
        do {
            var enabled: Int32 = 1
            try check(setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &enabled,
                                 socklen_t(MemoryLayout.size(ofValue: enabled))), "SO_NOSIGPIPE")
            var timeout = timeval(tv_sec: timeoutSeconds, tv_usec: 0)
            try check(setsockopt(descriptor, SOL_SOCKET, SO_SNDTIMEO, &timeout,
                                 socklen_t(MemoryLayout.size(ofValue: timeout))), "SO_SNDTIMEO")
            try check(setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &timeout,
                                 socklen_t(MemoryLayout.size(ofValue: timeout))), "SO_RCVTIMEO")
            return descriptor
        } catch {
            close(descriptor)
            throw error
        }
    }

    static func listen(at path: String) throws -> Int32 {
        var parent = stat()
        let directory = URL(fileURLWithPath: path).deletingLastPathComponent().path
        try check(lstat(directory, &parent), "lstat socket parent")
        guard parent.st_uid == geteuid(), parent.st_mode & S_IFMT == S_IFDIR,
              parent.st_mode & 0o777 == 0o700 else {
            throw ProbeError.invalid("Socket parent must be a real, owned 0700 directory")
        }

        var endpoint = try address(path)
        let descriptor = try open()
        do {
            let result = withUnsafePointer(to: &endpoint) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            try check(result, "bind")
            try check(chmod(path, 0o600), "chmod socket")
            try check(Darwin.listen(descriptor, 8), "listen")
            return descriptor
        } catch {
            close(descriptor)
            throw error
        }
    }

    static func connect(to path: String) throws -> Int32 {
        var endpoint = try address(path)
        let descriptor = try open()
        let result = withUnsafePointer(to: &endpoint) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            let code = errno
            close(descriptor)
            throw ProbeError.system("connect", code)
        }
        return descriptor
    }

    static func readLine(from descriptor: Int32) throws -> Data {
        let deadline = DispatchTime.now().uptimeNanoseconds + UInt64(timeoutSeconds) * NSEC_PER_SEC
        var bytes = Data()
        while bytes.count < frameLimit {
            let now = DispatchTime.now().uptimeNanoseconds
            guard now < deadline else { throw ProbeError.invalid("Request deadline exceeded") }
            // Drain buffered replies without mutating socket timeout state after a peer closes.
            var readiness = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
            let remainingMilliseconds = Int32(max(1, (deadline - now) / NSEC_PER_MSEC))
            let ready = poll(&readiness, 1, remainingMilliseconds)
            if ready < 0 {
                if errno == EINTR { continue }
                throw ProbeError.system("poll", errno)
            }
            guard ready > 0 else {
                throw ProbeError.invalid("Request deadline exceeded")
            }
            var byte: UInt8 = 0
            let count = recv(descriptor, &byte, 1, MSG_DONTWAIT)
            if count < 0 && (errno == EINTR || errno == EAGAIN) { continue }
            guard count == 1 else { throw ProbeError.system("recv", count == 0 ? ECONNRESET : errno) }
            if byte == UInt8(ascii: "\n") { return bytes }
            bytes.append(byte)
        }
        throw ProbeError.invalid("Frame exceeds \(frameLimit) bytes")
    }

    static func writeLine(_ data: Data, to descriptor: Int32) throws {
        let frame = data + Data([UInt8(ascii: "\n")])
        try frame.withUnsafeBytes { buffer in
            guard let start = buffer.baseAddress else { return }
            var sent = 0
            while sent < buffer.count {
                let count = send(descriptor, start.advanced(by: sent), buffer.count - sent, 0)
                guard count > 0 else { throw ProbeError.system("send", errno) }
                sent += count
            }
        }
    }

    private static func check(_ result: Int32, _ operation: String) throws {
        guard result == 0 else { throw ProbeError.system(operation, errno) }
    }
}
