# ADR 0006: Audit-token-bound ad-hoc client identity

Status: Accepted mechanism, S7 partial

## Context

The owner chose ad-hoc distribution without Developer ID or an entitled
App Group. [PLAN.md §4.3](../../PLAN.md#43-ipc) still requires privileged
requests to authenticate the connecting client. A bundle identifier or
claimed path cannot establish that identity.

## Method

`spikes/s7-client-identity` compiles strict Swift 6 fixtures. Its server
checks `getpeereid`, retrieves `LOCAL_PEERTOKEN`, resolves the token through
`SecCodeCopyGuestWithAttributes` and performs dynamic `SecCodeCheckValidity`
against an exact `cdhash` requirement. The fixtures use the same signing
identifier but different executable code; both are signed ad hoc.

The socket uses mode 0600 inside an owned 0700 directory. Frames and read
deadlines are bounded. A socket-pair regression reproduces buffered replies
after peer close independently of code-signature validation.

## Results

[Run 39024824695](https://github.com/L-K-M/Foldover/actions/runs/39024824695),
commit `ea06f55`, macOS 27.0.1, arm64, Xcode 27.0:

| Case | Result |
|---|---|
| Trusted client, privileged request | Permitted; UID and audit token checked, signature status 0 |
| Different code, same identifier | Denied; UID/token valid, signature status -67050 |
| Interpreter client | Denied; UID/token valid, signature status -67050 |
| Lookalike, public `daemon.status` | Permitted by user ID only |
| Relocated copy of trusted code | Permitted; signature status 0 |
| Unknown method | Denied |
| Malformed request | Denied |
| Socket and parent modes | 0600 / 0700 |
| Socket path | 62 UTF-8 bytes |
| Buffered reply after peer close | Regression passed |

Both executables reported `Signature=adhoc`, no Team ID, the same identifier
and different code hashes. All seven authorization cases passed.
`full_s7_passed` remained false.

The buffered-reply regression failed in
[run 39022836104](https://github.com/L-K-M/Foldover/actions/runs/39022836104):
per-byte receive-timeout updates returned EINVAL despite valid logical
timeout fields. Polling against one monotonic deadline passed the regression
and allowed the authorization measurements to complete.

## Decision

Use kernel peer credentials and audit-token-bound dynamic code validation
for privileged local requests. Fail closed if either identity observation
is unavailable. Public hello/status remain user-gated. Use a private,
short per-user socket directory without assuming App Group entitlement.

The production trust set must come from verified installed artifacts, not
request fields, paths, PIDs or client-supplied hashes. The harness's trusted
fixture argument is not that production trust mechanism.

## Remaining gates

- Installed-bundle trust, allowlist rotation and app/agent updates.
- Hardened Runtime, debugging and library-injection policy for shipping code.
- Different-user rejection and maximum/long UTF-8 socket paths.
- TCC attribution and guided access for Full Disk Access, removable/network
  volumes and Local Network, including permission behavior after updates.
- `SMAppService` registration and the clean-install flow.

This establishes an authorization primitive. It does not close S7 or grant
the agent filesystem permissions.
