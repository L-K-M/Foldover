# S7: Ad-hoc client identity

Run on macOS 27 with Xcode 27:

```sh
mkdir -p target
python3 spikes/s7-client-identity/run.py target/s7-results.json
```

The harness compiles strict Swift 6 server/client fixtures and ad-hoc-signs
two different clients with the same identifier. The server listens on a
0600 Unix socket inside an owned 0700 temporary directory. It checks the
peer user, obtains `LOCAL_PEERTOKEN`, resolves that audit token with
`SecCodeCopyGuestWithAttributes`, and uses dynamic `SecCodeCheckValidity`
against the trusted fixture's exact code hash.

Cases cover the trusted client, a different binary claiming its identifier,
an interpreter, a relocated copy of the trusted code, user-gated status,
unknown methods and malformed requests. Socket paths are bounded by the
SDK's `sun_path` capacity; reads have a frame limit and monotonic deadline.
A native socket-pair regression checks buffered replies after a peer closes.

`authorization_checks_passed` reports only these cases. `full_s7_passed`
remains false: TCC, Full Disk Access, removable/network volumes, Local
Network permission, different-user access, installed bundle updates and
agent registration still need their own measurements.

This prototype takes a trusted fixture path from its harness. Production
must derive the trust set from the installed bundle and validate its seals;
it must not accept a client-supplied path, PID, bundle identifier or code
hash as authority.
