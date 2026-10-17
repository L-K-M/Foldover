import Darwin
import Foundation

@main
private struct IdentityClient {
    static func main() throws {
        let arguments = CommandLine.arguments
        guard arguments.count == 3 else {
            throw ProbeError.invalid("client SOCKET METHOD")
        }
        #if LOOKALIKE
        let variant = "lookalike"
        #else
        let variant = "trusted"
        #endif
        let descriptor = try ProbeSocket.connect(to: arguments[1])
        defer { close(descriptor) }
        let request = try JSONSerialization.data(withJSONObject: ["method": arguments[2], "variant": variant])
        try ProbeSocket.writeLine(request, to: descriptor)
        let reply = try ProbeSocket.readLine(from: descriptor)
        FileHandle.standardOutput.write(reply + Data([UInt8(ascii: "\n")]))
    }
}
