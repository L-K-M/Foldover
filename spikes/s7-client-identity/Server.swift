import Darwin
import Foundation
import Security

private enum Check: String, Codable {
    case passed, failed, unavailable, notRequired
}

private enum Access: String, Codable {
    case permitted, denied
}

private struct Decision: Codable {
    let method: String
    let peerUID: UInt32?
    let user: Check
    let auditToken: Check
    let signatureStatus: OSStatus?
    let access: Access
    let reason: String
}

private struct Request: Decodable {
    let method: String
}

private func success(_ status: OSStatus, _ operation: String) throws {
    guard status == errSecSuccess else { throw ProbeError.security(operation, status) }
}

private func requirement(for executable: String) throws -> SecRequirement {
    var code: SecStaticCode?
    try success(SecStaticCodeCreateWithPath(URL(fileURLWithPath: executable) as CFURL,
                                          SecCSFlags(), &code), "static code")
    guard let code else { throw ProbeError.invalid("No static code") }
    try success(SecStaticCodeCheckValidity(code, SecCSFlags(rawValue: kSecCSStrictValidate), nil),
                "trusted fixture validity")
    var information: CFDictionary?
    try success(SecCodeCopySigningInformation(code, SecCSFlags(rawValue: kSecCSSigningInformation),
                                             &information), "signing information")
    guard let values = information as? [String: Any],
          let hash = values[kSecCodeInfoUnique as String] as? Data, !hash.isEmpty else {
        throw ProbeError.invalid("No trusted code hash")
    }
    let hex = hash.map { String(format: "%02x", $0) }.joined()
    var requirement: SecRequirement?
    try success(SecRequirementCreateWithString("cdhash H\"\(hex)\"" as CFString,
                                              SecCSFlags(), &requirement),
                "code-hash requirement")
    guard let requirement else { throw ProbeError.invalid("No requirement") }
    return requirement
}

private func decide(_ descriptor: Int32, method: String, required: SecRequirement) -> Decision {
    var user = uid_t.max
    var group = gid_t.max
    guard getpeereid(descriptor, &user, &group) == 0, user == geteuid() else {
        return Decision(method: method, peerUID: nil, user: .failed, auditToken: .notRequired,
                        signatureStatus: nil, access: .denied, reason: "peer user mismatch or unavailable")
    }
    if method == "daemon.status" || method == "session.hello" {
        return Decision(method: method, peerUID: user, user: .passed, auditToken: .notRequired,
                        signatureStatus: nil, access: .permitted, reason: "user-gated public method")
    }
    guard method == "preview.list" else {
        return Decision(method: method, peerUID: user, user: .passed, auditToken: .notRequired,
                        signatureStatus: nil, access: .denied, reason: "unknown probe method")
    }

    var token = audit_token_t()
    var length = socklen_t(MemoryLayout<audit_token_t>.size)
    guard getsockopt(descriptor, SOL_LOCAL, LOCAL_PEERTOKEN, &token, &length) == 0,
          length == MemoryLayout<audit_token_t>.size else {
        return Decision(method: method, peerUID: user, user: .passed, auditToken: .unavailable,
                        signatureStatus: nil, access: .denied, reason: "peer audit token unavailable")
    }
    let attributes = [kSecGuestAttributeAudit as String: withUnsafeBytes(of: token) { Data($0) }]
    var guest: SecCode?
    let resolved = SecCodeCopyGuestWithAttributes(nil, attributes as CFDictionary,
                                                SecCSFlags(), &guest)
    guard resolved == errSecSuccess, let guest else {
        return Decision(method: method, peerUID: user, user: .passed, auditToken: .passed,
                        signatureStatus: resolved, access: .denied, reason: "audit-token lookup failed")
    }
    // Dynamic validation binds the requirement to this socket's live peer, not a PID or claimed path.
    let validity = SecCodeCheckValidity(guest, SecCSFlags(), required)
    return Decision(method: method, peerUID: user, user: .passed, auditToken: .passed,
                    signatureStatus: validity, access: validity == errSecSuccess ? .permitted : .denied,
                    reason: validity == errSecSuccess ? "exact code identity" : "code identity rejected")
}

@main
private struct IdentityServer {
    static func main() throws {
        let arguments = CommandLine.arguments
        guard arguments.count == 4, let count = Int(arguments[3]), count > 0 else {
            throw ProbeError.invalid("server SOCKET TRUSTED_EXECUTABLE CONNECTION_COUNT")
        }
        let required = try requirement(for: arguments[2])
        let listener = try ProbeSocket.listen(at: arguments[1])
        defer {
            close(listener)
            unlink(arguments[1])
        }
        let output = FileHandle.standardOutput
        output.write(Data("{\"ready\":true}\n".utf8))
        for _ in 0..<count {
            let descriptor = accept(listener, nil, nil)
            guard descriptor >= 0 else { throw ProbeError.system("accept", errno) }
            defer { close(descriptor) }
            let decision: Decision
            do {
                let request = try JSONDecoder().decode(Request.self, from: ProbeSocket.readLine(from: descriptor))
                decision = decide(descriptor, method: request.method, required: required)
            } catch {
                decision = Decision(method: "malformed", peerUID: nil, user: .unavailable,
                                    auditToken: .notRequired, signatureStatus: nil,
                                    access: .denied, reason: "request rejected: \(error)")
            }
            let data = try JSONEncoder().encode(decision)
            try ProbeSocket.writeLine(data, to: descriptor)
            output.write(data + Data([UInt8(ascii: "\n")]))
        }
    }
}
