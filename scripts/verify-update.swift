// Check a Sparkle update signature against the public key the app ships
// (ADR 0035). sign_update signs with whatever private key it is given,
// so keys from two different pairs would publish a release no installed
// copy could ever take.
//
//   verify-update ARCHIVE SIGNATURE PUBLIC_KEY
//
// SIGNATURE and PUBLIC_KEY are base64, as sign_update and generate_keys
// print them. Exits 1 when the signature does not match.
import CryptoKit
import Foundation

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("verify-update: \(message)\n".utf8))
    exit(1)
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard arguments.count == 3 else {
    FileHandle.standardError.write(Data("usage: verify-update ARCHIVE SIGNATURE PUBLIC_KEY\n".utf8))
    exit(2)
}

guard let archive = FileManager.default.contents(atPath: arguments[0]) else {
    fail("cannot read \(arguments[0])")
}
guard let signature = Data(base64Encoded: arguments[1]) else {
    fail("the signature is not base64")
}
guard let keyBytes = Data(base64Encoded: arguments[2]),
      let key = try? Curve25519.Signing.PublicKey(rawRepresentation: keyBytes) else {
    fail("SPARKLE_PUBLIC_KEY is not an EdDSA public key")
}

guard key.isValidSignature(signature, for: archive) else {
    fail("the signature does not match SPARKLE_PUBLIC_KEY: SPARKLE_PRIVATE_KEY is from another key pair")
}
