# Hardware validation: real NAS and second Mac

The nightly device matrix covers file systems through disk images
(`scripts/device-matrix.sh`, ADR 0038). What it cannot image — a real
SMB/NFS appliance's name handling and a second Mac's peer transport —
stays manual (PLAN.md §5.2 "Test lab"). Run this checklist once per
release candidate; log the outcome and the build's version beside it.

A check that fails is a release blocker, except rows that record a
finding instead (§A.6); a row you cannot run is a known gap — record
either under the run's date, do not silently skip.

## Release-candidate run sheet

Copy this section once per release candidate and fill it in. It records
what a run actually covered; a row left `NOT RUN` is a known gap, not a
pass. Filling it in creates no release candidate and asserts no result.
Sections A and B are the owner checklists; this sheet holds the per-run
evidence and the remaining qualification in one place. Prior
measurements and their limits are in
[perf/qualification-results.md](../perf/qualification-results.md).

### Run identity

| Field | Record |
|---|---|
| Date, operator, RC label | |
| Version (`CFBundleShortVersionString`), source revision, `foldover --version` | |
| Bundle SHA-256 (app, `foldoverd`, `foldover` CLI) and signing (`codesign --verify --deep --strict`, `Signature=adhoc`, no Team ID) | |
| Hardware, OS, filesystems, protocols (NAS model/firmware, Mac models, macOS, media; APFS/HFS+/exFAT/FAT32; SMB/NFS/SFTP/peer) | |
| Cache state and load (cold/warm OS caches, aged catalog, NAS load average, RTT) | |
| Scope: installed app / agent+CLI / engine harness / service test | |

Record the installed bundle with:

```sh
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' Foldover.app/Contents/Info.plist
shasum -a 256 Foldover.app/Contents/MacOS/Foldover \
  Foldover.app/Contents/Library/LaunchServices/foldoverd \
  Foldover.app/Contents/Library/LaunchServices/foldover
codesign --verify --deep --strict --verbose=2 Foldover.app
codesign -d --verbose=4 Foldover.app 2>&1 | grep -E 'Identifier|Signature|TeamIdentifier'
```

### Results

One row per check. Scope is `installed app`, `agent/CLI`, `engine
harness` or `service test`; a check that mixes scopes is two rows. Result
is `PASS` (evidence attached), `FAIL` (blocker, with repro) or `NOT RUN`.
Engine-harness and service-test rows do not qualify the installed app.
Budgets are
[PLAN.md §5.3](../PLAN.md#53-performance-budgets-initial-calibrated-by-the-spikes);
harness commands are in [perf/qualification.md](../perf/qualification.md).

| # | Check | Scope | Result | Evidence and limitations |
|---|---|---|---|---|
| RC-1 | Initial 1M NAS baseline, phases timed | installed app | NOT RUN | Distinguish an empty-target copy from matching pre-generated folders; keep separate from warm runs. |
| RC-2 | Warm tracked 1M NAS no-change run, zero copies/updates/archives | installed app | NOT RUN | Record observation, apply and total; label warm tracked. |
| RC-3 | App process cold start to cached view < 300 ms | installed app | NOT RUN | Warm storage cache; repeat and record the median. |
| RC-4 | Cold storage cache preview vs warm | installed app | NOT RUN | State the cache condition; separate from RC-3. |
| RC-5 | Two or more concurrent held previews; status latency | installed app | NOT RUN | Record max handler and poll latency. |
| RC-6 | Agent peak RSS at 1M tracked files ≤ 500,000,000 bytes | agent/CLI | NOT RUN | Strict decimal; kernel peak RSS is authoritative. |
| RC-7 | Deep and Unicode paths copy byte-exact; no rename loop | installed app | NOT RUN | Composed and decomposed names, case-only renames. |
| RC-8 | NAS metadata fidelity: tags, comments, resource forks, ACLs | installed app | NOT RUN | Update [guide/nas-and-sftp.md](guide/nas-and-sftp.md) with what survives. |
| RC-9 | Cancel mid-run; undo newest run; no partial success | installed app | NOT RUN | Check cancellation and undo separately; verify content, archives and metadata outcomes. |
| RC-10 | Disconnect/reconnect: share drop or SSH loss, then next run | installed app | NOT RUN | Scheduled pair waits; no history entry while waiting. |
| RC-11 | SSH stall observation during the NAS runs | installed app | NOT RUN | Phase, duration, cause; retain failed artifacts. |
| RC-12 | Upload throughput ≥ 100 MB/s on 1 GbE, large files | installed app | NOT RUN | Record MB/s, file set and conditions. |
| RC-13 | Owner only: sleep/wake, Wi-Fi off/on, VPN or network move | owner | NOT RUN | Section B.3; agents do not simulate these. |
| RC-14 | Owner only: VoiceOver pass | owner | NOT RUN | [PLAN.md §10](../PLAN.md#10-decisions-for-the-owner). |

### Nightly and simulation evidence

| Field | Record |
|---|---|
| Nightly run links for the RC window: fuzz, simulate, peer chaos, devices matrix | |
| Simulation case counts and seeds (`FOLDOVER_SIMULATION_CASES`, `FOLDOVER_MEMORY_SIMULATION_CASES`) | |
| 1.0 gate: consecutive nights ≥100k seeded runs each, zero invariant violations | |
| Crash observation coverage: journal-transition kills and peer chaos runs ([ADR 0038](adr/0038-simulation.md)) | |
| Violations, seeds and repros | |

### Second Mac and cleanup

Install the same build on both Macs. A run where one Mac has an older
build is a compatibility result only; label it as such and do not count it
as RC qualification. A separate local account is optional; the existing
second Mac account may be used with dedicated synthetic folders, without
replacing its app or catalog, running its personal saved pairs, or
altering its personal schedules.

Before the run, record the exact owned fixture paths, the pair, share and
device IDs this run creates, and a read-only baseline of existing pair configurations, schedules,
shares, pins and relevant preferences. Keep that baseline private; do
not copy a live SQLite database without its consistency mechanism.
After the run, remove exactly what this run created:

- `foldover peers revoke <DEVICE>` for the test pairing.
- `foldover shares remove <ID>` for test shares.
- `foldover pairs remove <ID> <REVISION>` for test pairs.
- The Keychain item `ch.lkmc.foldover.peer-identity` / `local-device`,
  only if this run created it and nothing personal needs it.
- Only the exact fixture directories and test share listed above.

Verify the owner's pairs, schedules, shares and preferences are
unchanged, and investigate any unexpected change before attempting recovery.
Do not replace an existing catalog or restore an old whole-catalog
backup over newer activity. Never delete a pre-existing identity, pair,
share or schedule.

## A. Real NAS over SMB

Setup: mount the share in Finder (`smb://nas.local/…`), save the
credentials in Keychain, then pick the folder under `/Volumes/<share>`
for one side of a pair. Repeat every row in this section with an NFS
mount (`nfs://nas/…`) if the NAS offers both.

1. Preview a folder of a few hundred items with mixed content.
   - [ ] Scan completes; preview lists creates/copies/deletes correctly.
   - [ ] Names with accents, spaces and emoji preview and copy
         byte-exact (ambiguous `._*` AppleDouble files are reported and
         left unchanged while other files sync; see
         `docs/guide/nas-and-sftp.md`).
2. Run the sync.
   - [ ] Run completes; NAS side holds the copies.
   - [ ] A second run previews as "no changes" — nothing was written
         that the next scan disagrees with.
3. Live folder behavior.
   - [ ] Add a file on the NAS through Finder; the next run or
         "When files change" check sees it within its polling interval.
   - [ ] Edit a file on both sides → conflict is surfaced; resolve with
         Keep Both; both versions land in the archive/conflict copy.
4. Deletion review.
   - [ ] Delete files on the Mac side; the preview marks them for
         deletion on the NAS only after review.
5. Disconnect handling.
   - [ ] Unmount the share, run → the pair reports the side missing and
         writes nothing (ADR 0027).
   - [ ] Scheduled pair: unmount, wait one interval → status shows
         `waiting`, no history entry; remount → it runs at the next
         check without a manual poke.
   - [ ] While a copy runs, pull the network → the run fails with an
         error, never a partial write reported as success; on the next
         run the interrupted file is re-copied, not skipped.
6. Metadata fidelity (unverified behavior is a finding, not a crash).
   - [ ] Record which of tags, comments, resource forks and ACLs
         survive; update `docs/guide/nas-and-sftp.md` with what the
         appliance actually preserves.
   - [ ] Case-only renames on a case-insensitive share stay stable
         across runs (no rename loop).

## B. Second Mac (peer sync)

Setup: install the same build on both Macs on the same LAN.

1. Discovery and pairing.
   - [ ] The second Mac appears under "Servers nearby" / peers list.
   - [ ] Pairing shows six digits on both Macs; confirming both pins
         the device; cancelling either side pairs nothing.
   - [ ] `packet capture` or `foldover` logs show TLS only — no
         plaintext file data (PLAN.md P5 gate).
2. Sync.
   - [ ] A pair with a peer-side folder previews and syncs; a second
         run previews clean.
   - [ ] Files copy both directions; conflicts resolve as on local.
   - [ ] A large file copy that is interrupted resumes where it
         stopped (ADR 0034) — verify a `.foldover` partial entry is
         not left behind after it completes.
3. Network churn.
   - [ ] Toggle Wi-Fi off/on mid-run → the run fails cleanly or
         recovers on the next run; nothing hangs.
   - [ ] Sleep one Mac mid-run → the other reports the peer gone
         promptly, and the next run after wake works.
   - [ ] Move one Mac between networks (or toggle a VPN) → reconnect
         and sync work.
4. Unpairing.
   - [ ] Removing the pin closes live channels; the peer disappears
         from choices and future runs fail cleanly.

## Recording results

For each run: date, build version/commit, hardware (NAS model +
protocol, Mac models + macOS), rows run, pass/fail per row, and for
every failure the repro plus whether it is a blocker or a documented
limitation.

Keep the filled-in results with the release candidate's tracking issue
(or append a dated section to this file) so each RC's evidence stays
findable.
