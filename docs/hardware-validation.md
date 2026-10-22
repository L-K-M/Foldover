# Hardware validation: real NAS and second Mac

The nightly device matrix covers file systems through disk images
(`scripts/device-matrix.sh`, ADR 0038). What it cannot image — a real
SMB/NFS appliance's name handling and a second Mac's peer transport —
stays manual (PLAN.md §5.2 "Test lab"). Run this checklist once per
release candidate; log the outcome and the build's version beside it.

A check that fails is a release blocker, except rows that record a
finding instead (§A.6); a row you cannot run is a known gap — record
either under the run's date, do not silently skip.

## A. Real NAS over SMB

Setup: mount the share in Finder (`smb://nas.local/…`), save the
credentials in Keychain, then pick the folder under `/Volumes/<share>`
for one side of a pair. Repeat every row in this section with an NFS
mount (`nfs://nas/…`) if the NAS offers both.

1. Preview a folder of a few hundred items with mixed content.
   - [ ] Scan completes; preview lists creates/copies/deletes correctly.
   - [ ] Names with accents, spaces and emoji preview and copy
         byte-exact (watch for `._*` AppleDouble stops; a `._*` filter
         rule is expected on volumes without xattrs —
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
