# ADR 0006: Audit-token-bound ad-hoc client identity

Status: Implemented transport policy, S7 partial

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

## Installed-code transport policy

The Rust `net` driver derives the installation from its own canonical
executable location inside a Foldover app bundle. It reads the app, agent
and CLI signing information through the system `codesign` tool, then checks
the whole bundle and nested code with Security.framework. Dynamic validation
also binds the running executable to its expected installed code hash.
Identifier/path matches alone never authorize a peer.

This enrolls an owner-trusted local installation. Authenticating its initial
distribution and updates remains the S6 distribution responsibility.

Require Hardened Runtime and reject debugging, JIT, unsigned executable
memory, DYLD-environment and disabled-validation/protection entitlements.
Pin the validated app code hash and peer requirements for the process
lifetime. Each private call resolves a fresh kernel audit token, checks the
live peer's exact code identity and revalidates the pinned bundle. A changed
installation fails closed until the process restarts and enrolls the current
verified bundle. There is no client-supplied trust argument or PID lookup.

The agent accepts the installed app/CLI identities; clients accept only the
installed agent identity. Clients verify before sending private parameters.
Only exact `session.hello` and `daemon.status` names bypass code validation.
Standalone/debug builds retain user-gated health access; private methods fail
closed when installed-code evidence is unavailable. Linux retains its user
boundary.

Signing-tool output and deadlines are bounded. Security.framework calls use
a bounded blocking pool whose permits survive caller timeouts. Diagnostics
stay on stderr; authorization replies contain no installation paths.

Portable tests cover ambiguous signing-output rejection and the public-method
allowlist. Apple-target cross-checks run locally. macOS CI runs real signed
process fixtures for installed app/CLI access, relocated identity,
same-identifier lookalikes, interpreters, changed bundles, restart rotation
and fake agents receiving no private payload. This transport policy does not
enable pair APIs or establish their method/root scope.

## Rebuilt agent registration

An ad-hoc helper's designated requirement includes its exact code hash.
On macOS 27.0.1, Service Management retains the generated launch constraint
under the helper's launchd label after unregistering and registering it
again. The API reports the service as enabled, but launchd rejects the
updated executable with `OS_REASON_CODESIGNING` and a spawn-constraint
mismatch. Retrying registration under the same label does not reliably
refresh that constraint.

The installed-app failure was observed in the native logs on 2026-10-23.
An isolated signed app reproduced it after replacing its entire app bundle
with a new inode. Changing the helper's launchd label caused the updated
helper to start immediately. Changing only the bundle version, helper path,
plist filename, or an explicit additional spawn constraint did not repair
the retained constraint.

The build now signs the helper first and stamps its code hash into the
packaged launchd label, `ch.lkmc.foldover.agent.<cdhash>`. The helper's signing
identifier remains `ch.lkmc.foldover.agent`; private IPC retains the exact
installed-code checks above. The package check verifies that the label
matches the signed helper. A Swift-only update can retain its helper's
label and still restart the agent to enroll the changed app identity.

At launch, loaded previous jobs authorize migration only when launchd
explicitly reports all of them enabled. Missing, mixed or unknown status
does not authorize registration, and a disabled or unapproved service is
left alone. Before registering the new helper, the native boundary stops
the legacy label and older code-hash labels, then verifies their absence.
A failure to stop one prevents a new agent from starting.

`scripts/test-agent-registration.py` compiles the production boundary into
a temporary signed app with a unique service namespace. It checks migration
from a legacy label, two consecutive helper updates, preservation of a
disabled previous job even with another stale enabled job, and absence of
the earlier jobs after each migration. It never registers Foldover's
production service or opens sync data.

## Full Disk Access recovery after updates

The app reminds the user to review Full Disk Access on the first launch of
each signed build. Its code hash distinguishes builds even when the version
number stays the same. Acknowledging the reminder records only that review,
not a verified permission grant. Unsigned or unidentifiable builds do not
infer permission status.

Apple provides [no supported Full Disk Access status API](https://developer.apple.com/forums/thread/835851).
The app does not probe the privacy database, silently revoke access, or
change its signing requirements to preserve a grant. Startup displays
guidance only. Settings retains the same recovery actions after dismissal.

The explicit Reset and Open Settings action invokes Apple's
[app-scoped reset command](https://developer.apple.com/documentation/xcode/resetting-access-to-protected-resources-in-macos),
`tccutil reset SystemPolicyAllFiles ch.lkmc.foldover`, with a bounded runtime.
Only a successful reset opens Settings. The user must remove stale Foldover
entries if necessary, add the current app from Applications, and grant access
again. The app cannot confirm that this happened or guarantee that macOS
removes every stale visual entry. The action is disabled during known active
work across the configured pairs, and until their statuses are known; the
guidance also asks the user to finish
or stop any sync before resetting. It never restarts the agent.

Store tests cover build acknowledgements, explicit recovery, errors and
concurrent requests with mocked permission actions. Native command tests
use harmless executables, and the UI fixture has inert reset/settings
actions. These tests never reset the developer's real privacy permissions.

## Remaining gates

- Shipping-app update lifecycle qualification and authenticated distribution.
- Shipping-bundle debugging/injection and permission behavior after updates.
- Different-user rejection and maximum/long UTF-8 socket paths.
- TCC attribution and guided access for Full Disk Access, removable/network
  volumes and Local Network, including permission behavior after updates.
- `SMAppService` registration and the clean-install flow.

This establishes an authorization primitive. It does not close S7 or grant
the agent filesystem permissions.
