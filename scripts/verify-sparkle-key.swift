import Foundation
import CryptoKit

// Verify an ephemeral CI seed against the public key embedded into the product.
let input = FileHandle.standardInput.readDataToEndOfFile()
guard let text = String(data: input, encoding: .utf8),
      let seed = Data(base64Encoded: text.trimmingCharacters(in: .whitespacesAndNewlines)), seed.count == 32,
      let expected = ProcessInfo.processInfo.environment["SPARKLE_PUBLIC_ED_KEY"],
      let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: seed),
      key.publicKey.rawRepresentation.base64EncodedString() == expected else {
    fputs("Sparkle signing seed/public key mismatch.\n", stderr)
    exit(1)
}
print("PASS: persistent signing seed matches embedded public key")
