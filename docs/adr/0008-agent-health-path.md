# ADR 0008: Health-only agent and native registration boundary

Status: Implemented, platform lifecycle validation partial

## Decision

Connect the agent and clients through the
[planned layers](../../PLAN.md#44-rust-workspace):
`foldoverd → service` plus `ipc → net`, and
`CLI/UniFFI → client → ipc → net`.

The first live methods are `session.hello` and `daemon.status`. They check
the peer user, negotiate protocol/core versions, and report `HealthOnly`.
All other methods fail closed as unimplemented. No sync paths, file data,
pairs or history are exposed. Privileged methods require the installed-code
authentication and scope gates in [ADR 0006](0006-ad-hoc-client-identity.md).

Use bounded newline JSON-RPC frames, request deadlines, bounded connection
and batch counts, exact response IDs, and notification suppression. A private
0700 runtime directory, 0600 socket and exclusive runtime lock prevent
duplicate default agents. A stale socket is removed only after locking;
regular files and symlinks are refused. This is not yet the state-directory
writer lock required for persistence.

The macOS app embeds arm64 agent/CLI executables and a `BundleProgram`
LaunchAgent plist. Helpers are signed ad hoc with Hardened Runtime.
`FoldoverSystem` owns `SMAppService`; stores alone import it and FoldoverCore.
The UI offers explicit registration, approval guidance and health refresh.
Quitting the app does not stop the separately launched agent.

## Verification

`cargo xtask agent-smoke` builds the exact Cargo-reported executables and
checks the real CLI/agent lifecycle: startup, handshake, duplicate rejection,
unknown/malformed requests, notification/null-ID handling, unavailable-agent
failure, SIGKILL restart and clean SIGTERM shutdown. The smoke also runs
inside `cargo xtask ci`. Unit tests cover wire validation, framing, runtime
directory protections and service dispatch.

macOS CI additionally runs the smoke on the signed embedded executables and
checks the bundle's architecture, minimum OS, nested signatures and plist.

## Remaining gates

Clean-Mac registration/approval, TCC attribution, update/downgrade, crash-loop
safe mode, installed-client authentication, persistent writer ownership,
subscriptions, reconnect and all sync operations remain required. Packaging
or health success does not close S6, S7, P2 or P3.
