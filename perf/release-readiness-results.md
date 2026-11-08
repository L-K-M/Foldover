# Release-readiness qualification

The follow-up fixes are merged in [PR 271](https://github.com/L-K-M/Foldover/pull/271)
and [PR 272](https://github.com/L-K-M/Foldover/pull/272). The isolated peer
qualification fixture is merged in [PR 273](https://github.com/L-K-M/Foldover/pull/273).
[PR 274](https://github.com/L-K-M/Foldover/pull/274) adds the lease-wait
cancellation fix and corrects discovery and shutdown in that fixture.
The original performance
recommendations and their comparisons remain in
[qualification-results.md](qualification-results.md). This report adds
installed-app evidence; it does not declare a release qualified.

## Conditions

The primary Mac was an M4 Max with 36 GiB RAM, macOS 27.0.1 (26A434). The
second Mac was an M1 with 16 GiB RAM on the same macOS build. The hosts were
shared, OS caches were warm or uncontrolled, and the NAS route used Wi-Fi.
These are not idle-host, cold-storage or wired 1GbE measurements. Fixtures
contained only generated data. The matching million-file trees held one
million 1 KiB files and 1,000 directories on each populated side. The NAS
“baseline” here establishes the tracked catalog with the optimized build; it
is not a completed comparison against the original unoptimized binary.
Earlier interrupted comparisons remain in the performance report.

The real Release app was installed normally with its bundled agent and CLI,
normal authentication and the default catalog. That catalog and installation
were absent before qualification and were claimed for the test. No home,
state, IPC-authentication or production-source override was used. The GUI was
closed and relaunched during some background runs. Manually launched agent
sessions used the same installed binary and are labelled separately from
SMAppService lifecycle checks. The recorded binary hashes identify each build. The long NAS, local metadata
and initial memory runs used the descriptor-fixed build from PR 271. Final
installed cancellation checks used the later candidate containing PR 272 and
the lease-wait fix from PR 274; these are not presented as reruns of the
entire matrix on one binary.

CLI status latency includes starting the CLI and its IPC round trip; it is not
an isolated service-handler duration. Sampled RSS is not kernel peak RSS.
The strict memory comparison uses decimal bytes and the kernel resource report.

## Results

| Check | Result | Evidence and limit |
|---|---|---|
| Local matching 1M initial preview | PASS | 195.309 s, zero changes; subsequent baseline apply 46.159 s, zero writes/failures. |
| Local tracked 1M warm manual preview | PASS | 46.606 s, zero changes; maximum sampled CLI status round trip 126.975 ms. This is a full manual comparison, not a five-second event-driven quiet check. |
| Isolated 1M agent memory | PASS | Kernel peak 486,735,872 bytes, below 500,000,000. Manually timed installed agent with the real GUI; one active local pair. |
| Final candidate local 1M memory | PASS | The final installed candidate completed a zero-change full manual preview in 43.513 s; kernel peak was 488,079,360 bytes, below 500,000,000. The real GUI ran with a freshly timed stock agent; other pairs were idle. Peak includes the subsequent configuration-cleanup attempt and ownership audit. Maximum sampled CLI status round trip was 199.311 ms. |
| Multiple held previews | PASS | Local 1M and deep 1k previews held while NAS 1M work ran; maximum status round trip 81.383 ms across 20 polls. Sampled agent RSS reached 510,509,056 bytes; this is a larger workload, not the isolated 1M budget. |
| Deep/Unicode metadata copy | PASS | 1,000 files plus 54 directories copied. Independently verified every file's SHA-256, mode and mtime, 210 attributes, 10 resource forks and 10 ACLs. |
| Changes, case-only rename and undo | PASS | Update/delete/rename applied, followed by an empty warm preview. Undo restored original target bytes, mode, mtime, attributes, ACL and spelling; the source remained changed. |
| Large SFTP copy | PASS, bounded scope | 256 MiB in 8.218 s (32.7 decimal MB/s end to end); independent remote SHA-256 matched. Wi-Fi and verification overhead prevent a wired-throughput claim. |
| SFTP metadata contract | PASS | Data, mode 0640 and whole-second mtime matched. Tags, comments, quarantine and custom attributes were omitted as documented. A resource-fork copy failed without publishing its destination; the supported file then had no update loop. Server-inherited ACLs were not classified. |
| Preview cancellation | PASS | The interrupted NAS preview returned to idle in 0.489 s. Its 75-minute partial scan is not a completed million-file qualification. |
| Installed mutating cancellation | PASS | The final SMAppService agent stopped in 0.732 s after 8,454,144 staged bytes were independently observed. It reported cancellation with zero failures, removed staging and left the final target absent. A normal 256 MiB retry completed in 15.593 s with a matching SHA-256. SFTP uploads restart. |
| Installed lease-wait cancellation | PASS | The final SMAppService agent stopped in 2.013 s after observing a synthetic holder. It reported cancellation with zero failures, preserved the holder’s lock, wrote no target data and committed no base entries or first-sync identities. |
| Matching 1M NAS baseline | PASS | 6,337.348 s, zero changes/failures, 1,001,000 independently counted catalog entries. Maximum CLI status round trip 473.912 ms across 3,041 polls. |
| Tracked 1M NAS warm run | PASS | 5,622.680 s, zero changes/failures; 1,001,000 independently counted catalog entries. Maximum CLI status round trip 495.792 ms across 2,718 polls. The same SMAppService process survived both complete NAS runs. |
| App process cold start to cached view | NOT QUALIFIED | AX catalog readiness was 526–547 ms in one three-launch set. Concurrent AX polling produced 1.116–1.949 s, with launch callbacks at 78–95 ms; the probe can contend with the app. An Instruments diagnostic measured initial frame rendering, not cached-catalog readiness, and had a PID mismatch and sampling warning. No 300 ms budget pass is claimed. |
| Second-Mac peer backend | PASS, bounded scope | Normal code comparison, mutual pins, Unicode files, supported metadata, a no-change warm preview, reverse changes, undo and refusal of an unoffered share passed against the isolated M1 backend. The primary stock agent ran from Terminal; the M1 fixture retained a live SSH session. This does not qualify the current installed GUI on the M1. |
| Peer disconnect and retry | PASS, bounded scope | A 64 MiB transfer was interrupted after more than 20 MiB reached staging. A 22,413,312-byte partial matched the source prefix; no final target appeared. Restarting the same identity/transport endpoint and retrying produced a matching final hash and removed the partial. Wire-byte reuse was not measured. |
| Current installed peer initialization | NOT QUALIFIED | The final SMAppService agent returned Keychain error -25293 before starting its peer link. Adding only that exact agent to the task-created item through normal Keychain Access and retrying from a fresh process did not resolve it. The identity was never replaced or exported; it was removed only during final cleanup because it had been absent before the task. |
| Current installed peer revocation | NOT QUALIFIED | Revocation needs a running peer link and remained blocked by the same Keychain error. Cleanup removes the task fixture pin with the exclusive, originally absent test catalog; this is not reported as a successful public revocation. |

The first installed deep-tree preview failed with file-descriptor exhaustion:
361 entries failed under launchd's default soft limit of 256. PR 271 sets an
8,192-descriptor limit on Foldover's user agent. The native regression opens
1,024 descriptors in both parent and child before frameworks can raise the
limit, and the actual 48-level installed-app copy then completed cleanly.

The native quarantine regression was a real metadata defect: Darwin's ACL-only
`fcopyfile` path rewrote quarantine, and restoring it after applying a restrictive
ACL could fail. The replacement uses descriptor-based ACL APIs. The resource-fork
work-bound and restored-parent permission tests now assert their documented
contracts. All 91 filesystem tests and the four-filesystem device matrix passed.

## Retained failures and limits

The earlier NAS preview failed after 161.912 s with an SSH read error. A later
SMAppService preview was deliberately cancelled after about 75 minutes to
install and check the descriptor fix. Neither is a completed paired comparison.
The first combined timed agent session peaked at 604,340,224 bytes while
workloads overlapped; the isolated memory pass does not erase that observation.

Earlier peer attempts from SMAppService and from the same stock binary launched
under Paseo timed out. Pairing later succeeded with the primary agent launched
in Terminal and the M1 fixture held in a live SSH session. Both launch contexts
changed, so these observations do not isolate the cause. No global network or
privacy settings were changed. The missing-Team-ID log message does not prove
the cause of the earlier timeout.
Apple documents different permission handling for command-line tools and
launchd agents, and warns that ad hoc signatures can make identity tracking
unreliable. [Apple TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)

Normal configuration removal during final cleanup returned an error. A
subsequent command found that pair already absent, and a read-only audit
verified that every remaining pair and pin belonged to this task. The original
error cause is not assigned; this cleanup error is not a failed sync sample.

The first peer receiver-stop attempt exposed a test-fixture shutdown panic
that discarded staging. The fixture now drains incoming handlers while Tokio
is alive; its regression failed before the fix and passed afterward. Another
attempt was cancelled while waiting for a lease and never began copying.
Neither attempt is counted as a successful resume test. The first final SFTP
retest also lacked an in-flight observation because the independent control
SSH command timed out; the separately observed repeat supplies that evidence.

The second Mac's existing account, app and catalog remain intact. Its fixture
uses a private generated identity, an explicitly expected device/share and
normal TLS, code comparison and pinning. It cannot qualify the old installed
app as the current release candidate. The qualification network must permit
multicast/Bonjour for ordinary automatic discovery of the fixture.
The fixture marker is a convention,
not a sandbox against a concurrent process running as the same user.

## Cleanup

The M1's existing version 0.1.0 installation, pair revision, devices, pending
pairings and shares were checked unchanged before removing its separate
fixture. The generated M1 and NAS roots were deleted. Only the task's exact
SSH public-key line was removed, preserving all other authorized-key bytes
and metadata; the local temporary SSH key directory was removed too.

On the primary Mac, the task's service was unregistered and its jobs checked
absent. The originally absent app and catalog, temporary Keychain item and
marked local corpora were removed. Existing preferences were restored and
compared as parsed plists; import retained one additional key, which was
explicitly removed before that equality check passed. The stock ZIP was
sealed before a cleanup-only helper replaced the test app's executable for
unregistration; that helper was never packaged as the candidate. The task's
apps and dialogs were closed. The task's Terminal window was already gone;
its preexisting Terminal application and windows were preserved.

## Evidence and remaining release gates

The [sanitized evidence archive](results/release-readiness-2026-11-08.tar.gz),
[case index](results/release-readiness-index.csv) and
[SHA-256 manifest](results/release-readiness-manifest.json) retain both
successful and unsuccessful runs, actual revisions and binary hashes. Private catalogs, identity keys, SSH
configuration, GUI captures and raw packet traces are excluded. Each exported
payload has a SHA-256 manifest; archive ownership metadata is anonymous.

The [hardware guide](../docs/hardware-validation.md) lists the remaining owner
checks: same-build installed app on the second Mac, VoiceOver, physical
sleep/wake and network transitions,
cold storage and wired throughput. The nightly history and beta duration inform
the release decision; one completed extended run is not fourteen new nights.
The [final source extended run](https://github.com/L-K-M/Foldover/actions/runs/42519566139)
also passed on `4f1490b`: 500,000 parser cases, 5,000 filesystem histories,
100,000 in-memory histories and peer chaos.
The final stock qualification ZIP is kept locally as
`build/release-qualification/rc/Foldover-0.2.0-qualification.zip`
(15,704,091 bytes; SHA-256
`40d84b3c11c35e52685faaef70f2731fa0ca5f3d1630cff21194013832feb662`).
Its arm64 executables, strict ad hoc signatures, minimum macOS version,
agent label/resource policy and bundled notices were checked, and every
archived executable matched its installed counterpart. It was built from
`8fe4c1d2a7b1cb8446d7c0507630cb921784e883` plus the exact patch recorded
in the archive; that patch was verified against committed code
`4f1490b7dc9b9370bc3b125614a35c47871d3696`. Older prepared artifacts have
different hashes and are identified separately in the evidence.
No release tag or publication is part of this work.
