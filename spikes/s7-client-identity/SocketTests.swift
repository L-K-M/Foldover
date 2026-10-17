import Darwin
import Foundation

@main
private struct SocketTests {
    static func main() throws {
        var descriptors: [Int32] = [-1, -1]
        guard socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors) == 0 else {
            throw ProbeError.system("socketpair", errno)
        }
        defer {
            close(descriptors[0])
            if descriptors[1] >= 0 { close(descriptors[1]) }
        }

        let message = Data("buffered-reply".utf8)
        try ProbeSocket.writeLine(message, to: descriptors[1])
        close(descriptors[1])
        descriptors[1] = -1
        let received = try ProbeSocket.readLine(from: descriptors[0])
        guard received == message else {
            throw ProbeError.invalid("A peer close must not discard its buffered reply")
        }
        FileHandle.standardOutput.write(Data("buffered-reply-after-close: passed\n".utf8))
    }
}
