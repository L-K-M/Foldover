> Raw AI research report (opencode, GLM-5.3), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# Sync-Engine Correctness and Performance: Research Report for Foldover

**Date:** 2026-10-16 · **Scope:** reconciliation models, change detection, edge cases, performance, crash recovery, and a distilled design spec for a new macOS-native (Swift/SwiftUI) + portable-core (Rust) sync app targeting local disks, network shares, SFTP and LAN peers.

Every factual claim carries an inline `[n]` marker pointing to the Sources list at the end. Items I could not confirm are marked **UNVERIFIED**.

---

## 1. Executive summary

The evidence from two decades of shipped synchronizers converges on one architecture:

1. **Three-way reconciliation against a persistent "last-synced" base.** Unison's per-pair archive [2], Dropbox Nucleus's Remote/Local/Synced trees with the Synced Tree acting as a merge base [15], FreeFileSync's `sync.ffs_db` [17], GoodSync's per-job state files [23] and ChronoSync's Difference Engine database [26] are all the same idea: comparing *two current states to each other* cannot distinguish "deleted on the left" from "created on the right"; comparing *each current state to a remembered base* can.
2. **Event-driven watching plus periodic full verification scans.** Every event API coalesces or drops events and must be treated as a hint, not a log (FSEvents explicitly so [29]; inotify queue overflow [30]; Syncthing keeps hourly full scans even with the watcher on [8]).
3. **Atomic, idempotent mutations.** Write to a temp file in the destination directory, fsync (F_FULLFSYNC on macOS), rename over the target, fsync the directory [31][34]. This makes every operation crash-safe and re-runnable, which is what allows Unison's "safe to interrupt at any time" guarantee [2].
4. **Deletions are the dangerous case; gate them.** Mass-deletion thresholds (rclone bisync `--max-delete` default 50% [12], rsync `--max-delete` [7], Unison `confirmbigdel` [2]), empty-replica aborts [12], and ransomware canaries (ChronoSync HoneyPotChecker [28]) all exist because "unmounted source looks like mass deletion" is the classic data-loss bug.
5. **For N replicas, start with a star (hub) topology; adopt version vectors only when you ship a peer protocol.** Unison's manual explicitly recommends star topology for >2 machines because pairwise archives otherwise create spurious conflicts [2]; Syncthing's per-file version vectors are the proven mesh solution [9].

---

## 2. Reconciliation models of existing systems

### 2.1 Unison — archive-based three-way sync with a formal specification

Unison is a GPL file synchronizer for POSIX and Windows, developed since the late 1990s under Benjamin Pierce; the project is now hosted on GitHub and the old site is archival [1][3]. Current manual version studied: 2.54.0 [2].

**Data model.** Unison keeps an *archive file per replica pair* on each host, named from the canonical hostnames, replica paths and an internal format version [2]. The archive records the contents of each path "at the last moment when they were the same in the two replicas" [2]. An **update** is defined purely by *contents* (file bytes+perms, symlink target, directory token, or ABSENT) differing from the archive record — touching a file's mtime without changing content is not an update, and a file changed and changed back is not an update either [2]. A **conflict** is: updated in one replica, updated (or a descendant updated) in the other, and current contents not identical [2].

**Reconciliation.** Five stages: (1) compare archive vs. current on each host to find updates; (2) silently resolve *false conflicts* (updated on both sides but now identical — mark as synced); (3) show updates, suggest directions for non-conflicts; (4) perform actions "by first transferring the new contents to a temporary file on the receiving host, then atomically moving them into place"; (5) update archives [2].

**Invariants (quoted/paraphrased from the manual).** At every moment each path has either its original or its correct final contents; Unison's own state is either unchanged or updated to reflect completed synchronizations; it is "safe to interrupt Unison at any time" — with a documented caveat for file↔directory replacements, where a `DANGER.README` file is left in `.unison` [2]. If interrupted mid-propagation, the next run sees both sides changed-but-equal and self-heals [2]. Unison is explicitly not bothered by clock skew: it only compares timestamps obtained from the same host and assumes each clock runs forward [2].

**Fastcheck.** Default update detection on Unix uses modtime+length as a "pseudo inode number" instead of hashing every file; `fastcheck=default` means fast checks on Unix replicas, slow (content) checks on Windows replicas where mtime+size can survive an edit [2]. It "may sometimes detect false updates but will never miss a real one" under normal circumstances, though it can be fooled by tools like `retouch`; crucially, Unison "always does a safe check for updates just before propagating a change," so a missed update is never overwritten [2]. Fastcheck is automatically disabled for `.xls`/`.mpp` because Excel updates files without changing modtimes [2]. An experimental `fastercheckUNSAFE` pseudo-fingerprint mode exists and is documented as able to miss conflicts [2].

**Safety features relevant to Foldover:** `confirmbigdel` (default true) requires extra confirmation when an entire replica (or top-level path) appears deleted; in batch mode this aborts the run [2]. Centralized backups of overwritten files (`backupdir`, `file.N.unison.bak`) [2]. `copyonconflict` keeps a copy of the losing side when combined with `-repeat watch -prefer newer` for unattended operation [2]. `-repeat watch` (or `watch+3600`) drives continuous operation from a filesystem-monitoring process plus optional periodic full scans [2]. Rename/move detection exists as `-moves-experimental` [2]. macOS resource forks are stored as Carbon `._` AppleDouble files on foreign filesystems (`rsrc`), and extended attributes are synced (`-xattrs`) [2]. Unison does not understand hard links [2], does not sync setuid/setgid bits [2], and documents the pitfall that renaming a directory containing *ignored* files propagates as delete+create and destroys the ignored files [2].

**N replicas.** "Unison is designed for synchronizing pairs of replicas… the most reliable way is a star topology" with one hub, precisely because a separate archive per pair otherwise produces spurious conflicts [2]. Archive loss or a format change after upgrade degrades safely: everything unknown becomes "new", so unequal files become conflicts rather than overwrites [2].

**Formal specification.** Pierce & Vouillon's *"What's in Unison? A Formal Specification and Reference Implementation of a File Synchronizer"* (2004; UPenn MS-CIS-03-36) presents "a simple, precise specification of Unison, phrased in terms that both implementors and users can understand," extending an earlier specification by Balasubramaniam and Pierce [4][5]. The Unison manual itself points to the papers for rigorous treatments of the concepts [2]; the rsync-style transfer in Unison is credited to Tridgell's thesis work [2][6].

### 2.2 Syncthing — version vectors, block exchange, mesh sync

Syncthing is an open-source continuous synchronizer; documentation studied at version v2.1.0. (Docs render "Written for v2.1.0" as of 2026-10-16.)

**Model.** Each device holds a *local model* (metadata + block hashes per folder) and computes a *global model* — the union of all devices' files, selecting the highest change version — and converges toward it [9]. The **Block Exchange Protocol (BEP)** runs over TLS 1.3 with certificate-fingerprint ("Device ID", SHA-256 of the X.509 cert) authentication [9].

**Versioning.** Every `FileInfo` carries a **version vector**: repeated `Counter{id, value}` pairs where `id` is the first 64 bits of a device ID and `value` an incrementing counter; "(Folder, Name, Version) uniquely identifies the contents of a file at a given point in time" [9]. A per-folder monotonic `sequence` plus a random 64-bit `index_id` implements *delta index exchange*, so reconnecting peers only send changed index entries [9]. Deletions are **tombstones**: the `deleted` flag with zero-length block list and the deletion (or last-known) modification time [9].

**Content addressing.** Files are split into blocks of 128 KiB–16 MiB (powers of two; chosen so a file has <2000 blocks), each block hashed with SHA-256; requests are offset+size+hash and the responder verifies or may serve an identical block from a *different file* [9]. Syncthing's docs describe exactly this: a needed block "might be locally [available], if another file already has a block with the same hash" — which is why renames, although represented as delete+create, cost no network transfer [8][9]. Blocks are written to a temp copy (`.syncthing.<name>.ext.tmp`, `~syncthing~` on Windows), verified by re-hashing, then moved into place; temp files are kept up to a day and peers advertise partial block availability (`DownloadProgress`, every 5 s) so interrupted transfers resume at block granularity [8][9]. The BEP message-size limit is 500,000,000 bytes [9]. Names are mandated to be UTF-8 NFC on the wire regardless of OS conventions [9].

**Change detection.** Watcher enabled by default plus a full scan every hour (actual interval randomized between 3/4 and 5/4 of the setting to desynchronize folders); during rescans files are checked for mtime/size/permission changes and only "rehashed" (re-block-listed) when those changed; the watcher accumulates changes for 10 s and delays deletions by 1 minute [8].

**Conflicts.** Concurrent modifications with different content rename the loser to `<filename>.sync-conflict-<date>-<time>-<modifiedBy>.<ext>` (older mtime loses; ties broken by the larger first 63 bits of the device ID); modify-vs-delete conflicts that resolve toward deletion also keep a conflict copy; conflict copies then sync as ordinary files [8]. Syncthing is case-sensitive by design and reports case clashes on case-insensitive systems as errors; before v1.9.0 it could access one file under two case-differing names, which was fixed [8].

**Versioning/trash.** Per-folder strategies — Trash Can, Simple (keep N), Staggered (30 s → 1 h → 1 day → 1 week retention tiers), External command — stored in `.stversions`, applied only to changes arriving *from other devices* [10]. Ignore patterns live in `.stignore` (first match wins; `(?i)` case-insensitive prefix — always on for Mac/Windows; `(?d)` prefix permits deletion of ignored files blocking directory removal; `#include` and `!` negation supported) [11].

**Reception.** Long-running HN threads show sustained adoption: the 2014 launch thread (623 points) [42], a 2021 thread (545 points) [43], LWN's "Syncthing: Syncing All the Things" (433 points) [44]; the official Android app was discontinued in 2024 (418-point thread), with community forks carrying it on [45].

### 2.3 rsync — delta transfer and the "quick check"

rsync is one-directional (it cannot propagate updates back), but its mechanics are foundational. Its "quick check" skips files whose size and mtime match [7]; the famous **delta-transfer algorithm** "reduces the amount of data sent over the network by sending only the differences between the source files and the existing files in the destination," and rsync uses "pipelining of file transfers to minimize latency costs" [7]. The rolling-checksum basis comes from Tridgell's 1999 thesis *Efficient Algorithms for Sorting and Synchronization*, which the Unison manual cites as the basis of its own rsync-style protocol [2][6].

Options that encode hard-won correctness lessons [7]:
- `--modify-window=NUM` (`-@`): treat timestamps as equal within NUM seconds — "specifying 1 is useful for copies to/from MS Windows FAT filesystems, because FAT represents times with a 2-second resolution"; a negative value (rsync ≥3.1.3) compares nanoseconds.
- `--inplace`: writes directly to the destination — breaks hard links, cannot update in-use binaries, "the file's data will be in an inconsistent state … if the transfer is interrupted."
- `--append`/`--append-verify`: resume growing files, explicitly "can be dangerous" unless all files are known append-only.
- `--partial`/`--partial-dir=DIR`: keep partially transferred files and resume from them next run.
- `--sparse` (`-S`): map null runs to holes (with `lseek` `SEEK_HOLE`/`SEEK_DATA` semantics implied by the sparse model; see copy_file_range notes below [32]).
- `-H` hard links are only detected *within* the transfer set.
- `--backup`/`--backup-dir`: move overwritten/deleted files aside before replacing.
- `--max-delete=NUM`: cap deletions per run.
- `--delay-updates`, `--fuzzy` (find a similar basis file), `--link-dest` (hardlink-identical snapshots), `--fsync` (fsync each written file).
- Security: `--safe-links`/`--munge-links` against symlink traversal, and an explicit warning that copying from case-preserving to case-insensitive filesystems can make a symlink name "overlap" a file name "which can cause potentially dangerous results such as files being written outside the destination directory hierarchy" [7].

### 2.4 rclone bisync — listing-based state

`bisync` is rclone's bidirectional mode (marked "advanced… or data loss can result"). It stores **prior listings** (path, size, modtime, optionally checksum) under `~/.cache/rclone/bisync`, one per path-side, and on each run computes the delta of each side against its own prior listing [12].

Safety machinery worth copying [12]:
- First run (or recovery) requires `--resync` (default winner: Path1; `--resync-mode` can pick newer/older/larger/smaller/path2).
- `--max-delete` aborts the run if more than a percentage of files would be deleted — **default 50%** — including the note that renaming a directory containing >half your files trips it.
- `--check-access` requires matching `RCLONE_TEST` marker files on both sides before syncing.
- An empty *current* listing on either side aborts ("Empty current PathN listing. Cannot sync to an empty directory") precisely so an unmounted/failed side doesn't wipe the other.
- An "All files changed" check aborts when every prior-existing file's timestamp changed (e.g., a timezone change).
- The filters file is hash-gated (MD5 stored alongside); editing filters forces a `--resync` so newly-excluded files aren't seen as deletions.
- End-of-run `--check-sync` compares the two final listings for integrity.
- Since v1.66 a "snapshot" model handles files changing mid-run: missed changes roll to the next run, with an internal recheck flag [12].

Conflicts (new/changed on both sides *and* currently non-identical) are never silently resolved by default: both copies are renamed (`file.txt.conflict1` / `.conflict2`); `--conflict-resolve newer|older|larger|smaller|path1|path2` picks a winner and `--conflict-loser num|pathname|delete` disposes of the loser; suffixes support date template variables including a `{MacFriendlyTime}` that avoids illegal `:` characters in macOS filenames [12]. The default comparison is size+modtime; content-identical files are not treated as conflicts even if touched on both sides [12].

The SFTP backend documents practical transport realities: modification times are stored with 1-second precision; the SFTP RFC packet payload cap is 32 KiB (many servers, including OpenSSH, tolerate up to a 256 KiB total packet, so 255k chunks dramatically improve high-latency throughput); rclone pipelines up to 64 outstanding requests per file; SFTP defines no server-side copy (rclone can optionally emulate one with hardlinks) [13].

### 2.5 Dropbox Nucleus — the reference architecture

**"Rewriting the heart of our sync engine"** (Sujay Jayakar, Mar 9, 2020) and **"Testing sync at Dropbox"** (Isaac Goldberg, Apr 20, 2020) describe Nucleus, the Rust rewrite shipped to all users in 2020 [14][15].

Data model — the core of the design [15]:
- The legacy engine ("Sync Engine Classic") keyed its database (SQLite) by full path, so a rename was a delete at the old path plus an add at the new — for a folder, O(n) deletes/adds; worse, a network hiccup could land the delete but not the add, leaving the file missing on the server and every other device [14][15].
- Nucleus persists *observations* as three trees: **Remote Tree** (server state), **Local Tree** (last observed disk state), and **Synced Tree** ("the last known 'fully synced' state between the Remote Tree and Local Tree"). "If you're familiar with version control, you can think of each node in the Synced Tree as a merge base" — it derives the *direction* of a change (locally edited vs. remotely edited) and without it you cannot tell a remote add from a local delete [15].
- Nodes have globally unique identifiers, so a move is a metadata update, executed on disk as a single atomic rename; enforced invariant: "any moved folder is visible in exactly one location" [15]. Protocol-level invariant: "no file or folder can exist (even transiently) without a parent directory" [15].
- The **planner** consumes the three trees and emits batches of operations safe to run concurrently (children after parents; sibling edits parallel) [15].

Engineering lessons: bidirectional sync has "many corner cases" and offline-then-reconcile is *normal* operation for a sync engine, not an anomaly [14]; specifying correct behavior is genuinely hard (their nested-move example produces a filesystem *cycle*: Archives→January→Drafts→Archives) [14]. Nucleus runs almost everything on a single deterministic "control thread" using Rust futures, offloading I/O and hashing; determinism plus a PRNG seed makes bugs reproducible [14].

**Testing** [15]: *CanopyCheck* fuzzes the planner: generate one random tree, perturb into the other two, repeatedly ask the planner for a batch, shuffle execution order, pretend ops succeeded, assert convergence — plus invariants (termination cutoff ≈200 planning iterations; no panics; "if a file exists only in the Remote Tree it must exist in all three trees at the end"), and QuickCheck-style minimization by shrinking trees. *Trinity* runs the full engine against in-memory filesystem/network/time mocks, reordering responses, injecting failures and simulated crashes, then re-runs the same seed to verify determinism (≈10× slower in native mode); *Heirloom* exercises the real server protocol (≈100× slower). Dropbox runs "tens of millions of randomized test runs" nightly [15].

### 2.6 FreeFileSync — database files, file-ID move detection

FreeFileSync (open source; free for personal use, Business Edition €20/device with one year of updates, as listed on its FAQ checked 2026-10-16 [16]) creates **`sync.ffs_db` database files** after the first two-way sync and compares current filesystem state against them to determine directions [17]. Its standout feature is **moved-file detection**: when "Use database file to detect changes" is enabled *and the filesystem supports file IDs*, moves are applied as moves on the target instead of copy+delete; this requires the second sync onward, is impossible on FAT (unstable IDs) and SFTP, and falls back to copy+delete there [17]. Sync variants: Two way / Mirror / Update / Custom (rules over categories or over detected create/update/delete) [17].

Comparison variants: *file time and size* (categories include "conflict (same date, different size)"), *file content* (bitwise, slowest, "fallback when modification times are not reliable"), *file size* only; symlinks can be followed or copied *as link* [18]. DST handling: FAT stores local time vs NTFS UTC, so ±1 h shifts appear after DST changes; the fix is explicit "time shifts to ignore" entries in comparison settings [19]. Versioning: deletion handling can be Permanent / Recycle Bin / Versioning folder with `Replace`, `Time stamp [File]` (`File.txt 2020-12-11 111111.txt`) or `Time stamp [Folder]` naming [20]. Performance: parallel file operations configured **per device root** (folders grouped by device; a pair uses the max of its two sides), with the caveat that SFTP servers limit concurrent connections [21]. RealTimeSync triggers runs from OS change notifications after a user-configurable idle period, and honestly documents that some network drivers "just do not receive any" notifications [22]. The FAQ also lists Windows VSS for locked files, >260-char path support, ~1 million file pairs per GB of RAM, and DST/case-sensitive handling as features [16].

### 2.7 GoodSync — per-side state comparison, conflict taxonomy

GoodSync (Siber Systems; proprietary) documents its algorithm outline: compute the difference between current state and **stored file state** for the Left folder, same for Right, propagate each side's changes, and declare a Conflict when the same file changed on both sides; deletions propagate correctly "through multiple chained folders," and a deliberate replacement of a newer file by an older one is propagated rather than ignored [23]. State lives in a `_gsdata_` folder beside the synced folder (state files, logs, previous versions), with a "Delete State Files" command and even a "No ListDir (trust state file)" option [23]. Its **conflict taxonomy** is instructive: *Modified on both sides* (resolvable), *Vertical Delete–Change* (folder deleted on one side, contents changed on the other; resolvable), *Case Conflict* ("items differ only by case… non-resolvable conflict, the user must correct the problem"), *ACL/Extended-attributes conflict* [23]. Dedicated actions include Copy Time (same length, different mtime), **Copy Name Case** (case-only renames propagated without copying content), and Sync State (repair recorded state when files match but state doesn't) [23]. Automation ("On File Change", default 20 s delay; ~3 s polling quant; sync starts after 5 minutes of continuous changes) notes it works on Windows shares "except for some faulty NAS shares that do not report changes" [23].

### 2.8 ChronoSync — the Mac-native incumbent

ChronoSync (Econ Technologies) is $49.99 with free lifetime updates and requires macOS 10.14+ (product page checked 2026-10-16) [24]. Its documented core, the **ChronoSync Difference Engine (CDE)**, is "1) a database containing a snapshot of a pair of file systems and 2) a set of algorithms used to analyze and maintain that snapshot," stored inside the synchronizer document [26]. The CDE is credited with detecting what naive comparers miss: both-sides conflicts (instead of silently overwriting with the newer file), **date rollbacks** (file replaced by an older version), **filename-case changes** (NOTES.txt→Notes.txt propagated "in the direction that was intended"), and attribute degradation on servers/NAS (it reads back attributes after copying and records what the remote filesystem actually stored, avoiding pointless recopy loops) [26]. **SmartScan** (v4.6+) accelerates analysis by querying the macOS kernel's per-volume fsEvents database as a *hint* layered on the CDE; it requires volumes that expose fsEvents (local disks and ChronoAgent connections — "file servers and removable media will not qualify"), needs "Admin Access" on macOS 12+, documents a "phantom folder replacement" failure mode (a moved folder replacing a near-identical one with identical mtime), mitigates it with a failsafe that periodically distrusts fsEvents, and offers an "aggressive mode" that skips the extra checks [27]. **HoneyPotChecker** is a pre-sync script that maintains a decoy document with an MD5 in an xattr and aborts the sync if it changed — a ransomware/mass-corruption canary [28]. HN sentiment (sporadic but positive): photographers describe set-and-forget tether backups and praise the always-free updates ("the product is always updated to all owners") and the "fat VERIFY button" [46][47].

---

## 3. Recommended model for Foldover

**Adopt per-pair three-way reconciliation against a persistent last-synced base, with content-hash verification — i.e., the Unison/Nucleus/FreeFileSync family, not the listing-diff family.**

Concretely, for each synced pair (A, B) Foldover maintains, inside its engine database:
- `snapshot(A)` and `snapshot(B)`: observed per-path records (kind, size, mtime_ns, file-id/inode, permissions, symlink target, content hash or block list).
- `base`: the recorded state at the last successful synchronization — Foldover's "Synced Tree" [15].

Classification per path is then the standard three-way lattice (identical logic to Unison's stages [2] and Nucleus's merge-base derivation [15]):

| base | A | B | action |
|---|---|---|---|
| X | X | X | in sync |
| X | X' | X | propagate A→B |
| X | X | X' | propagate B→A |
| X | X' | X' | if content(A)==content(B): mark synced (false conflict [2]); else **conflict** |
| — | X | — | propagate A→B (create) |
| — | — | X | propagate B→A |
| X | — | X | propagate deletion A→B (tombstone) |
| X | X | — | propagate deletion B→A (tombstone) |
| X | — | — | already deleted both sides |
| — | X' | X'' | conflict (both new, different) |

Deletions only ever propagate from a side that *matches the base* — this is the precise sense of the invariant "never delete a file whose current content was not previously synced" (a file absent from the base was never synced, so its absence cannot be a propagated deletion).

**Cheap upgrades over the pure model:**
- **File-ID move detection** (FreeFileSync-style) when both endpoints expose stable IDs (APFS inode numbers, NTFS file IDs): a disappeared path plus an appeared path with the same ID and unchanged content is a move [17]. On endpoints without IDs (SFTP, FAT), fall back to hash-based rename detection (Unison's `-moves-experimental` [2]) or Syncthing-style block reuse, which makes moves cheap to *transfer* even when detected as delete+create [8].
- **Case-only renames** as a first-class action (GoodSync's "Copy Name Case" [23]; ChronoSync's CDE handles them [26]); on case-insensitive filesystems execute as a two-step rename (`a.txt` → temp name → `A.txt`).

**Extending to N replicas.** Two proven shapes:
1. **Hub/star (recommended for MVP).** Each non-hub replica syncs pairwise with the hub (an always-on Mac, a NAS share, or an external disk). This is Unison's explicit recommendation because per-pair state otherwise creates spurious conflicts [2], and GoodSync documents the equivalent "chained synchronization" pattern (A↔USB↔B) as a first-class use case with correct deletion propagation through the chain [23]. Foldover's MVP targets (disks, shares, SFTP) are all "dumb endpoints," so pairwise-with-hub is the only implementable topology anyway.
2. **Mesh with version vectors (when a Foldover↔Foldover LAN protocol ships).** Adopt Syncthing's design: per-file version vector of `(device_id, counter)` pairs, merged by dominance; equal-footing vectors with different content = conflict copy; tombstoned deletes carry the version vector forward; delta index exchange via per-folder monotonic sequence numbers [9]. This removes the hub as a bottleneck and matches user expectations for Mac-to-Mac sync. (Dropbox's globally-unique node IDs that survive moves [15] are the server-mediated analogue; without a server, version vectors are the decentralized version.)

---

## 4. Change detection

### 4.1 Event APIs are hints, not logs

- **macOS FSEvents** delivers *directory-level* notifications: events may coalesce (kFSEventStreamEventFlagMustScanSubDirs), and kernel/user "dropped" flags mean "you must do a full scan of everything you're monitoring because there is no way to determine what may have changed" [29]. Apple's own guidance: treat the event list "as advisory rather than a definitive list," and "backup software should still periodically perform a full sweep of any volume to ensure that no changes fall through the cracks" [29]. Persistent per-volume event IDs let you resume after downtime (store the last-processed event ID; per-device streams keyed by volume UUID) [29]. ChronoSync's SmartScan is the production embodiment: fsEvents as a hint over a snapshot database, with a failsafe that periodically re-verifies untouched directories [27].
- **Linux inotify** (for the later Linux port): watches are per-directory and non-recursive; the queue can overflow (IN_Q_OVERFLOW → events lost → rescan); `max_user_watches`/`max_user_instances`/`max_queued_events` need tuning for large trees; renames arrive as IN_MOVED_FROM/IN_MOVED_TO pairs joined only by a racy cookie value; and — decisive for Foldover — "it does not catch remote events that occur on network filesystems. (Applications must fall back to polling…)" [30]. Watchman's docs give the same story and their overflow strategy: "assume that all the files have been modified and … re-crawl" [40].

**Design consequence:** always run watcher + periodic full scan (Syncthing defaults: watcher on, hourly randomized full scan [8]; Unison: `repeat watch+3600` [2]; FFS RealTimeSync: notifications with idle-time debounce [22]).

### 4.2 Fast-path metadata vs. content hashing

The universal fast path is: a file whose (size, mtime, and where available inode/file-id) matches the recorded state is treated as unchanged; anything else is hashed. rsync's "quick check" is exactly size+mtime [7]. Unison's `fastcheck` adds the crucial safety property: even when the fast path *misses* an edit (same size+mtime), a **safe re-check just before propagating** prevents the stale copy from overwriting the missed change — fold this "re-verify target before overwrite" step into Foldover [2]. Also fold in Unison's observation that some applications (Excel) edit files without touching mtime, and its remedy of occasional slow (content) passes [2]; rclone bisync offers the same trade-off via `--compare checksum` and honestly documents that content changed without mtime/size change is invisible to the default mode [12].

ctime is not exposed on macOS the way Linux exposes it and is unreliable across network filesystems; treat it as a hint only (compare against the spec: FSEvents is authoritative-by-rescan [29], SFTP gives 1-second mtimes [13]). On APFS, mtimes have nanosecond resolution [35]; on FAT, 2-second resolution with the DST/local-time trap — rsync's `--modify-window=1` exists precisely for this [7], and FreeFileSync implements explicit "ignore time shift" values [19].

### 4.3 Network shares: no events, listing-driven scans

For SMB/NFS/AFP mounts and SFTP there is no usable event stream: inotify explicitly excludes network filesystems [30]; ChronoSync excludes file servers from fsEvents-based SmartScan ("file servers and removable media will not qualify") [27]; FreeFileSync's RealTimeSync documents NAS drivers that silently never notify [22]; GoodSync notes "faulty NAS shares that do not report changes" [23]. Therefore:

- **Poll by directory enumeration, not per-file stat.** One directory listing that *includes* attributes is one round trip; a stat per file is one round trip per file. NFSv3 defined this pattern (`READDIRPLUS` "returns file handles and attributes in addition to directory entries" [39]); SMB2's directory query likewise returns file information classes with each entry (**UNVERIFIED here** — the [MS-SMB2] spec pages were not retrievable during research; treat SMB compounding/multi-credit as performance background, not a verified claim). On Linux `readdir` over SMB (smbfs) commonly returns only names, forcing a stat storm — when Foldover later ships a native SMB client, prefer enumeration calls that carry attributes.
- **Pipelining and parallelism over SFTP:** rclone's defaults (32 KiB payload per RFC cap, up to 255 KiB total packets on OpenSSH, 64 outstanding requests per file, 1-minute idle connection recycling) are a good calibration point [13]; keep concurrency high for latency, bounded for connection limits [21].
- **Scale targets:** Everything (Windows, reading the NTFS MFT + USN Journal) indexes ~1M files in about a minute using ~100 MB RAM [41] — evidence that full scans of million-file trees are feasible when metadata-dense enumeration is available, and that a persistent change journal (USN) is what makes incremental rescans cheap on Windows.

### 4.4 Watching while scanning

Start the watcher *before* scanning and rescan any directory that changes mid-scan (Apple's explicit guidance, to avoid the start/stop race) [29]; Syncthing's 10 s watcher debounce plus 1-minute deletion delay is a sane, shipped calibration [8]; GoodSync's On File Change uses a 20 s default delay and force-syncs after 5 minutes of continuous changes [23]; FreeFileSync waits for a configurable idle period before triggering [22].

---

## 5. Edge cases and safety

**Renames/moves.** Baseline: delete+create (that's what rsync does, and what Sync Engine Classic's O(n)-explosion bug taught Dropbox to avoid for directories [15]). Layer on: atomic `rename(2)` (POSIX guarantees "if newpath already exists, it will be atomically replaced" — with the documented window where both names refer to the file [31]); file-ID detection where IDs exist [17]; hash-based detection as a fallback [2]; block-hash reuse so moves never re-transfer bytes [8]; if a Foldover-to-Foldover protocol ever exists, Nucleus-style stable node IDs + single-atomic-move [15]. Note `rename()` across filesystems fails with EXDEV — moves between replicas must be copy+delete [31].

**Case-only renames on case-insensitive filesystems.** macOS filesystems are case-insensitive by default; Syncthing treats case variants as distinct and errors on clashes (fixed a real dual-access bug in v1.9.0) [8]; GoodSync declares case conflicts non-resolvable but propagates pure case changes via "Copy Name Case" [23]; ChronoSync's CDE attributes direction to case-only changes [26]; rsync warns that case-folding can even make a symlink name collide with a file name dangerously [7]. Foldover: detect case-only deltas explicitly (base differs from current only by case, other side at base) and implement as rename-via-temporary on case-insensitive volumes.

**Unicode normalization.** macOS is the hazard zone: HFS+ normalizes filenames (a form of Unicode normalization), while APFS stores names as raw bytes ("bag of bytes") and does not normalize, inheriting normalization-*insensitivity* comparisons — early APFS's lack of normalization broke non-English workflows against HFS+ habits [35][36][37]. Syncthing simply mandates UTF-8 NFC on the wire [9]. Foldover: store the exact on-disk byte name, compare names byte-wise *and* a normalized (NFC) key for equivalence, and canonicalize to NFC when creating files — so an NFD name arriving from one Mac doesn't fork into a second file on an APFS target.

**mtime granularity / clock skew / DST / FAT.** FAT: 2-second resolution → rsync's `--modify-window=1` [7]; FAT stores local time vs NTFS UTC → ±1 h phantom differences after DST or timezone changes; FreeFileSync solves it with explicit ignored time-shift values and recommends same-filesystem pairings [19]; rclone bisync hard-aborts when *all* prior files' timestamps change (timezone flip detection) [12]; SFTP mtimes are 1-second precision [13]. Clock skew between machines is a non-issue if, like Unison, you never compare timestamps *across* machines — only each side's current stat against your own recorded base [2]. Never use mtimes to order conflicting versions across replicas (Syncthing does use mtime for conflict-copy tie-breaking, with a deterministic device-ID tiebreak as second factor [8]).

**Files modified during copy.** Detect by stat-before/stat-after around the copy and re-queue (Unison: propagating a stale version "will signal a failure for that file. Run Unison again" [2]; rclone bisync v1.66+ uses a snapshot model with next-run recheck [12]; rclone additionally offers `--local-no-check-updated` semantics around this problem [12]). Debounce with an idle window before syncing hot files (Syncthing 10 s [8], FFS idle delay [22], GoodSync 20 s [23]). On macOS, writes from other processes are visible immediately — there are no mandatory locks to save you; open-with-write detection (e.g., attempting `open(O_EVTONLY)` or checking `lsof`-style state) is advisory at best. (Windows-only tools use VSS for locked files [16]; out of MVP scope.)

**Atomic replace.** Canonical sequence: create temp file in the *destination directory* (same filesystem, so rename won't EXDEV [31]); write; `fsync`; on macOS issue `F_FULLFSYNC` — plain fsync "may not physically write the data to the platters for quite some time and … may be written in an out-of-order sequence," and F_FULLFSYNC "asks the drive to flush all buffered data to permanent storage" [34]; `rename()` over the target [31]; fsync the directory (durability of the rename). This is what Unison ("transferring … to a temporary file … then atomically moving them into place" [2]), Syncthing (`.tmp` then move [8]) and rsync by default (temp-dir + rename; `--inplace` documented as the dangerous exception [7]) all do. Set final mtime *after* content is complete (else crashes leave misleading timestamps; cf. bisync's snapshot redesign [12]).

**Resuming interrupted large transfers.** Keep the temp file plus a journal of completed block hashes; on restart, hash what's there and fetch only the remainder (Syncthing: temp files kept ~1 day, peers advertise available blocks, requests by offset+size+hash [8][9]; rsync: `--partial-dir` as staging for later resumption [7]; SFTP: ranged parallel reads [13]).

**Deletion propagation.** Tombstones in state (Syncthing's `deleted` flag carrying deletion time [9]); deletion only when the other side matches the base (three-way lattice, §3). Guards:
- *Mass-deletion threshold*: abort/confirm when deletions exceed a fraction of the tree (rclone bisync default 50% [12]; rsync `--max-delete` [7]; Unison `confirmbigdel` [2]).
- *Empty/unmounted replica*: refuse to sync against an empty current listing (bisync aborts verbatim [12]); verify the replica root still exists and its volume UUID/device matches (FSEvents per-device guidance [29]; Unison's `mountpoint` preference exists for exactly this [2]).
- *Canary*: a hash-checked decoy file as a ransomware/corruption tripwire before syncing (ChronoSync HoneyPotChecker: MD5 in an xattr, pre-sync script, abort on mismatch [28]).
- Archive deletions into trash/versioning before removing (below).

**File/directory type conflicts.** If base says file and a side now has a directory (or vice versa), never overwrite; surface as conflict. Note Unison's documented exception: type-changing replacement is the one operation that isn't perfectly atomic, leaving `DANGER.README` if interrupted [2].

**Permission errors.** Degrade, don't halt: per-file error accumulation with a cap (Unison `maxerrors` [2]), then a clear failure report. Record what the destination *actually stored* after copying (ChronoSync reads back attributes to avoid re-copy loops on servers that can't represent them [26]) — Foldover should snapshot post-copy reality into the base, not the intended values.

**Symlinks and hard links.** Never follow a symlink that escapes the replica root (rsync `--safe-links`/`--munge-links` model [7]); offer follow vs. copy-as-link (FreeFileSync [18]); represent symlinks explicitly in state like BEP (type + target [9]). Hard links: don't promise preservation (Unison doesn't support them [2]; rsync's `-H` works only within one transfer [7]); they cannot be preserved across SMB/SFTP at all. Watch the case-folding symlink-collision hazard when syncing into case-insensitive volumes [7].

**Open/locked files.** macOS locks are advisory; copy anyway but flag mtime-stability for re-check next cycle (see above). Windows (post-MVP): VSS [16].

**Sparse files.** Copy hole-aware: `lseek(SEEK_HOLE/SEEK_DATA)` walks; note that `copy_file_range()` "may expand any holes existing in the requested range" so loop with SEEK_DATA/SEEK_HOLE [32]; rsync `--sparse` maps null runs back to holes [7]. Detect sparseness by `st_blocks * 512 < st_size`.

**Long paths.** macOS path components are limited by NAME_MAX (255) and APIs by PATH_MAX (1024); the practical long-path pain (260 chars) is Windows-specific and handled by tools like FreeFileSync [16] — out of MVP scope, but keep the core path-agnostic (byte strings, not UTF-16-with-max-260 assumptions).

**Disk full.** Preflight free space per operation batch; preallocate when beneficial (rsync `--preallocate` [7]); remember that APFS clones can *later* fail with ENOSPC on write because "the clonefile() system call might not allocate new storage for data blocks" [33]; surface ENOSPC per file and stop cleanly with the journal intact.

**Conflict policy and conflict-file naming.** Default keep-both with a discoverable suffix; name must be filesystem-safe (bisync even ships a `{MacFriendlyTime}` because `:` is illegal in macOS filenames [12]). Syncthing's `<filename>.sync-conflict-<date>-<time>-<modifiedBy>.<ext>` (older mtime loses; deterministic device-ID tiebreak; conflict copies sync as normal files so every replica ends with both) [8] is the best-specified precedent; rclone bisync's loser/suffix system (`num`, `pathname`, `delete`; date-templated suffixes) [12] and Unison's interactive resolve + optional `copyonconflict` [2] complete the design space. Avoid ever auto-resolving with mtime alone across machines (clock skew).

**Versioning / trash of replaced files.** Ship a trash folder (Syncthing `.stversions` with trashcan/simple/staggered retention [10]; FFS versioning with timestamp naming [20]; rsync `--backup-dir` [7]; Unison centralized `backupdir` [2]). On APFS make trashing free with clones — Finder copies already dedupe via clones and `cp -c` requests them [33][35]. Apply versioning to deletions *and* overwrites; make retention staggered by default (Syncthing's 30 s/hour/day/week tiers are a good default curve [10]).

---

## 6. Performance

**Parallelism should be per-device and workload-aware.** FreeFileSync's model is exactly right: parallel file operations are configured *per device root* (a sync pair runs at the max of its two sides' settings), because throughput ceilings and seek behavior belong to devices, not to jobs [21]. HDDs punish random access — especially APFS, which co-locates metadata with file data, making enumeration "much slower on APFS when it is located on a hard disk drive" as the read head seeks scattered fragments [38]. Calibrate: low concurrency (1–4) and larger sequential reads for HDD→HDD; high concurrency for SSDs; high concurrency for SMB/SFTP where per-request latency dominates.

**Pipelining.** rsync's core latency trick ("pipelining of file transfers to minimize latency costs" [7]) and rclone SFTP's 64 outstanding requests per file [13] both show that on high-latency links you must keep many requests in flight per connection rather than paying round trips; on SFTP also raise chunk size within the server's tolerance (32 KiB RFC cap; 255 KiB works on OpenSSH) [13].

**Block-level delta: when it helps vs. hurts.** Helps when the link is slower than re-reading the basis file locally — large files, small edits, WAN/SFTP, and resumption; Syncthing's fixed 128 KiB–16 MiB blocks with SHA-256 lists and cross-file block reuse [8][9] are a simple, proven scheme (no content-defined chunking needed for sync-as-opposed-to-backup; rename handling comes free via block reuse). Hurts on fast local disks — rsync itself defaults to whole-file copies for local transfers (`--whole-file` implied for local; the man page notes disabling it "usually slows rsync down" but minimizes destination writes) [7]; and delta computation costs a full read of both versions. Foldover should use blocks for *verification and resume* always, but compare transfer-vs-hash cost before doing delta assembly locally.

**APFS clones for local copies.** `clonefile(2)` (macOS 10.12+) gives O(1) copy-on-write clones within a volume, is atomic, and is checkable via `ATTR_VOL_CAPABILITIES`/`VOL_CAP_INT_CLONE`; Finder uses it for user copies and `cp -c` exposes it [33][35]. Use it for: trashing/versioning replaced files (instant, space-free), staging temp copies on the same volume, and same-volume replica pairs. Caveats: destination must not exist; later writes can hit ENOSPC [33].

**copy_file_range(2)** (Linux, for the future port): in-kernel copy without user-space bounce buffers; filesystems may accelerate it with reflinks or server-side copy (NFS) [32]. On macOS the equivalents are `fcopyfile(3)` and `clonefile(2)` [33]. Watch the sparse-hole expansion caveat [32].

**Buffer sizes.** The SFTP numbers above (32 KiB payload; up to ~255 KiB packets where the server tolerates) [13] and Syncthing's block ladder [9] suggest 128 KiB–1 MiB read/write buffers as the sweet spot for local I/O; larger buffers mainly help on very fast NVMe where syscall overhead matters — benchmark, don't guess.

**Verification cost.** Hashing is a full read of every changed file, twice if you verify both sides (Syncthing re-hashes every received block before writing [8]; rclone bisync warns that listing checksums on `local` must be computed on the fly and can "dramatically slow down" runs, hence `--no-slow-hash` [12]). Budget: hash in parallel with transfers (separate thread pool — Nucleus offloads hashing [14]); use BLAKE3 (Rust `blake3` crate) for speed, but SHA-256 if you want to be bit-compatible with Syncthing-style block lists later. Periodic full-content verification passes (Unison's occasional `fastcheck=false` run [2]) can be scheduled rather than paid every cycle.

**Batching metadata on SMB.** Enumerate with attributes in one call (the READDIRPLUS lesson from NFSv3 [39]); avoid per-file `stat`; keep one warm connection; prefer depth-first directory batching. (SMB2 request compounding as a specific mechanism: **UNVERIFIED** here.)

**Scanning millions of files.** Parallel walking with per-device limits [21]; event-hinted incremental scans [27]; randomized scan scheduling to avoid thundering herds (Syncthing randomizes intervals to 3/4–5/4 [8]); the Everything benchmark (~1M files/minute with dense metadata reads [41]) is the reference point for what a cold scan should feel like with native enumeration.

---

## 7. Crash recovery

1. **Journal planned operations before executing them.** Each journal entry: op type, target path, temp path, expected pre-state (base hash/file-id), expected post-state. The journal is a table in the engine's SQLite database (Nucleus's predecessor persisted its state in SQLite [15]); commit journal updates transactionally.
2. **Make every operation idempotent** so recovery = re-run: copy writes a temp file then renames (atomic [31][34]); mkdir is mkdir-if-absent; delete checks existence first; set-mtime re-issues harmlessly. This is precisely why Unison can promise that an interrupted run leaves every path with either its original or its final contents [2] and why Syncthing's partial temp files can be resumed or discarded [8].
3. **Startup reconciliation:** open the DB; for any journal entries in `planned`/`executing` state, verify the target's actual state (hash or file-id) and either complete or roll back; discard stale temp files older than the retention window (Syncthing keeps temps ~1 day [8]).
4. **Self-healing base drift:** if a crash happened between mutating a replica and committing the base update, the next cycle classifies that path as "changed on the executed side, matches base on the other" and propagates — the same self-heal Unison describes for interrupted propagations [2]. Because propagation is idempotent, re-propagating is safe (worst case it's a no-op via hash compare).
5. **Degrade loudly, not silently:** when integrity is uncertain (journal corruption, mid-write crash on a type-changing replacement), stop and require an explicit rebuild — the conservative lockout-plus-resync pattern of rclone bisync [12], and Unison's `DANGER.README` marker for the one non-atomic case it cannot fully protect [2].
6. **fsync discipline:** file fsync (F_FULLFSYNC on macOS [34]) before rename; fsync the containing directory after rename/mkdir/unlink; fsync the journal before considering an op committed (rsync exposes exactly this as `--fsync` [7]).

---

## 8. Design spec for Foldover (distilled)

### 8.1 Data model (engine database, one SQLite file per sync pair)

```sql
-- Observed state of one replica (also recorded for the other side)
CREATE TABLE snapshot (
  path        TEXT PRIMARY KEY,     -- relative path, NFC-normalized key + raw byte name
  kind        INTEGER NOT NULL,     -- file | dir | symlink
  size        INTEGER,
  mtime_ns    INTEGER,
  file_id     BLOB,                 -- APFS inode/NTFS file id; NULL if unsupported
  perms       INTEGER,
  symlink_tgt TEXT,
  content_hash BLOB,                -- whole-file hash (BLAKE3), computed lazily
  block_list  BLOB                  -- [(offset,size,hash)] when hashed
);

-- The last-synced base ("Synced Tree")
CREATE TABLE base (
  path        TEXT PRIMARY KEY,
  kind, size, mtime_ns, perms, symlink_tgt, content_hash,  -- as of last sync
  synced_at   INTEGER,
  origin      INTEGER               -- which replica produced the winning content
);

CREATE TABLE journal (
  op_id       INTEGER PRIMARY KEY,
  op          TEXT,                 -- copy|delete|mkdir|rmdir|rename|set_meta|conflict_copy
  path        TEXT, temp_path TEXT,
  pre         BLOB, post            BLOB,  -- expected states (JSON)
  state       TEXT                  -- planned|executing|done|failed
);

-- Future mesh support
CREATE TABLE version_vector (path TEXT, device BLOB, counter INTEGER,
                             PRIMARY KEY (path, device));
CREATE TABLE tombstones (path TEXT PRIMARY KEY, deleted_at INTEGER, vv BLOB);
```

Trash/versioning lives on disk as a sibling folder (`.foldover-versions/`), with APFS clones used when same-volume [33].

### 8.2 Algorithm (pseudocode)

```
function sync_cycle(pair A, B):
    lock(pair)                          # flock on a lock file in the engine dir
    gate: root_exists(A) and root_exists(B)
          and volume_id_unchanged(A,B)  # unmounted/empty-replica protection [12][29]
    gate: canary_ok(A) and canary_ok(B) # honeypot hash check [28]

    hint = events_since(last_event_id)  # FSEvents persistent ids [29]; empty on net fs
    a = scan(A, hint); b = scan(B, hint)  # watcher-before-scan; rescan changed dirs [29]
    # scan(): walk with enumeration-carrying-attrs; fast-path (size,mtime,file_id);
    #         hash only when changed or when due for slow pass [2][7]

    plan = []
    for path in union(a, b, base):
        (ba, xa, xb) = (base[path], a[path], b[path])
        if xa == ba and xb == ba:            continue            # in sync
        if xa != ba and xb == ba:            plan += propagate(path, A→B)
        elif xb != ba and xa == ba:          plan += propagate(path, B→A)
        elif content(xa) == content(xb):     mark_synced(path)                  # false conflict [2]
        elif xa is None and xb is None:      drop_from_base(path)
        else:                                plan += conflict_copy(path)        # keep both [8]
        # move detection: same file_id appeared+disappeared → rename op [17]

    gate: deletions(plan) <= 0.5 * entries(base)  else ABORT with report [12]
          # also confirm interactively unless unattended+auto rules allow [2]

    plan = topological_sort(plan)        # parents before children; siblings parallel [15]
    journal.append(plan)
    for batch in parallel_groups(plan, per_device_limits(A,B)):      # [21]
        for op in batch:
            re_verify_target(op)         # safe check immediately before mutation [2]
            execute_idempotent(op)       # temp+fsync(F_FULLFSYNC)+rename+dir_fsync [31][34]
        journal.mark_done(batch)
    base.commit(final_states(a', b'))    # read back actual stored attrs [26]
    verify(optional): sample hash-compare both sides                  # [12]
```

### 8.3 Invariants (must hold at every instant)

1. **No data loss without consent:** a file is overwritten or deleted only if its current content equals the base (i.e., it was previously synced and unchanged) — "never delete a file whose current content was not previously synced." (This is the operational form of Unison's "original or final contents" invariant [2] and Nucleus's merge-base discipline [15].)
2. **Atomicity:** every path observable at every instant holds either its original or its complete final content; only journal-managed temp names are ever partial [2][8].
3. **Idempotence:** re-running any committed-or-crashed operation after restart is safe and converges.
4. **Base integrity:** the base is only updated transactionally after both replicas reach the recorded state; a crash between mutation and base-commit self-heals on the next cycle [2].
5. **Containment:** the engine never reads or writes outside the replica root via symlink following [7]; temp and trash files live inside the replica or engine space and are excluded from sync.
6. **Deletion gates:** mass-deletion threshold, empty-replica abort, root/volume identity check, and canary check all run before any destructive op is journaled [12][28][29].
7. **Determinism (testability):** given identical inputs and schedule, planning produces identical output — the property Dropbox built CanopyCheck/Trinity on [14][15].

### 8.4 Test matrix

Axes (product, not all cells unique):

| Axis | Values |
|---|---|
| Filesystem A/B | APFS(SSD), APFS(HDD), HFS+, exFAT, FAT32, SMB(Samba), SMB(macOS server), NFS, SFTP(OpenSSH) |
| Operations | create, edit, touch-only, chmod/chown, rename same-dir, rename cross-dir, move cross-fs (EXDEV), case-only rename, delete, mass-delete (10%, 50%, 100%), dir↔file replacement, symlink create/retarget, hardlink group, sparse file, huge file (>block ladder), zero-length, xattr-only change, resource-fork change |
| Timing | change during scan; change during copy; watcher disabled; event overflow (drop events); crash points: before/after each journal state, before/after rename, before/after base commit |
| Environment | mtime truncation (1 s SFTP), DST transition, timezone change, clock skew ±hours, unmounted-at-start, volume reformatted (UUID change), disk-full at write, permission-denied dir, 1M-file tree |
| Topology | A↔B; chain A↔H↔B (hub); (future) 3-node mesh with version vectors — concurrent edit, concurrent delete, partition+rejoin |

Properties asserted per cell: the §8.3 invariants; convergence (both sides + base agree); no-conflict-lost (any two differing contents that both existed are both still recoverable — from trash or conflict copies); journal drains to empty after recovery; idempotent re-run is a no-op; and Dropbox-style randomized/property-based fuzzing with seed-reproducible schedules for the planner [15].

### 8.5 Implementation notes (Rust core)

Crates to evaluate (existence well-known; no behavior claims made here): `notify` (FSEvents/inotify abstraction), `walkdir`/`jwalk` + `rayon` (parallel scanning), `blake3`/`sha2` (hashing), `rusqlite` (state/journal, WAL mode), `tempfile`/`fd-lock`, `russh` + `russh-sftp` or `ssh2` (SFTP), and libsmbclient bindings or the OS SMB client via mounted volumes for SMB. macOS specifics: `F_FULLFSYNC` via `fcntl` [34]; `clonefile`/`fclonefileat` after checking `VOL_CAP_INT_CLONE` [33]; FSEvents per-device streams keyed by volume UUID [29]; App Sandbox with user-selected read/write folder access for arbitrary destinations (not re-verified in this research pass — **UNVERIFIED**, check current App Sandbox/TCC docs before designing the app's permission UX).

---

## 9. Sources

1. Unison project page (archival) — Benjamin C. Pierce, UPenn. https://www.cis.upenn.edu/~bcpierce/unison/
2. Unison User Manual, version 2.54.0 (plain text). https://raw.githubusercontent.com/bcpierce00/unison/documentation/unison-manual.txt
3. Unison GitHub repository and wiki. https://github.com/bcpierce00/unison/wiki
4. Benjamin C. Pierce and Jérôme Vouillon, "What's in Unison? A Formal Specification and Reference Implementation of a File Synchronizer" (PDF). https://www.cis.upenn.edu/~bcpierce/papers/unisonspec.pdf
5. Same paper — Semantic Scholar entry (published 2004; Corpus ID 6674906). https://www.semanticscholar.org/paper/4ea035290846ac68429049a482304f9b5ff452ba
6. Andrew Tridgell, "Efficient Algorithms for Sorting and Synchronization" (PhD thesis, 1999) — URL as cited by the Unison manual [2]. https://samba.anu.edu.au/~tridge/phd_thesis.pdf
7. rsync(1) manual page. https://download.samba.org/pub/rsync/rsync.html
8. Syncthing docs: "Understanding Synchronization" (v2.1.0 docs). https://docs.syncthing.net/users/syncing.html
9. Syncthing docs: "Block Exchange Protocol v1" (specification). https://docs.syncthing.net/specs/bep-v1.html
10. Syncthing docs: "File Versioning." https://docs.syncthing.net/users/versioning.html
11. Syncthing docs: "Ignoring Files" (.stignore). https://docs.syncthing.net/users/ignoring.html
12. rclone docs: "Bisync." https://rclone.org/bisync/
13. rclone docs: "SFTP." https://rclone.org/sftp/
14. Sujay Jayakar, "Rewriting the heart of our sync engine," Dropbox.tech, Mar 9, 2020. https://dropbox.tech/infrastructure/rewriting-the-heart-of-our-sync-engine
15. Isaac Goldberg, "Testing sync at Dropbox," Dropbox.tech, Apr 20, 2020. https://dropbox.tech/infrastructure/-testing-our-new-sync-engine
16. FreeFileSync FAQ (pricing/edition facts checked 2026-10-16). https://freefilesync.org/faq.php
17. FreeFileSync Manual: "Synchronization Settings." https://freefilesync.org/manual.php?topic=synchronization-settings
18. FreeFileSync Manual: "Comparison Settings." https://freefilesync.org/manual.php?topic=comparison-settings
19. FreeFileSync Manual: "Daylight Saving Time." https://freefilesync.org/manual.php?topic=daylight-saving-time
20. FreeFileSync Manual: "Versioning." https://freefilesync.org/manual.php?topic=versioning
21. FreeFileSync Manual: "Performance." https://freefilesync.org/manual.php?topic=performance
22. FreeFileSync Manual: "RealTimeSync." https://freefilesync.org/manual.php?topic=realtimesync
23. GoodSync Manual ("Synchronization algorithm," "Actions," "Conflict action," "On File Change," state files). https://www.goodsync.com/manual
24. ChronoSync product overview (price $49.99, macOS 10.14+, free updates; checked 2026-10-16). https://www.econtechnologies.com/chronosync/overview.html
25. ChronoSync Tech Notes index. https://www.econtechnologies.com/support/chronosync/tech-notes.html
26. Econ Technologies, "TNCS-0026 – ChronoSync Difference Engine." https://www.econtechnologies.com/chronosync/TN-CS-difference-engine.html
27. Econ Technologies, "TNCS-0025 – SmartScan." https://www.econtechnologies.com/chronosync/TN-CS-smartscan.html
28. Econ Technologies, "TNCS-0037 – HoneyPotChecker Script." https://www.econtechnologies.com/chronosync/TN-CS-HoneyPotChecker-script.html
29. Apple, File System Events Programming Guide (Documentation Archive). https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/UsingtheFSEventsFramework/UsingtheFSEventsFramework.html
30. inotify(7) — Linux man-pages project. https://man7.org/linux/man-pages/man7/inotify.7.html
31. rename(2) — Linux man-pages project. https://man7.org/linux/man-pages/man2/rename.2.html
32. copy_file_range(2) — Linux man-pages project. https://man7.org/linux/man-pages/man2/copy_file_range.2.html
33. clonefile(2) — Darwin/macOS man page (mirror). https://keith.github.io/xcode-man-pages/clonefile.2.html
34. fsync(2) — Darwin/macOS man page, incl. F_FULLFSYNC (mirror). https://keith.github.io/xcode-man-pages/fsync.2.html
35. Wikipedia, "Apple File System" (incl. references to Apple's APFS Guide/Reference). https://en.wikipedia.org/wiki/Apple_File_System
36. Michael Tsai, "APFS's 'Bag of Bytes' Filenames" (as referenced by [35]). https://mjtsai.com/blog/2017/03/24/apfss-bag-of-bytes-filenames/
37. Apple, "Apple File System Guide" (as referenced by [35]). https://developer.apple.com/documentation/foundation/file_system/about_apple_file_system
38. Bombich Software, "An analysis of APFS enumeration performance on rotational hard drives" (as referenced by [35]). https://bombich.com/blog/2019/09/12/analysis-apfs-enumeration-performance-on-rotational-hard-drives
39. B. Callaghan, B. Pawlowski, P. Staubach, RFC 1813 "NFS Version 3 Protocol Specification," June 1995 (READDIRPLUS, §3.3.17). https://www.rfc-editor.org/rfc/rfc1813.html
40. Meta/watchman documentation: "Installation" (inotify limits, FSEvents, overflow strategy). https://facebook.github.io/watchman/docs/install.html
41. voidtools, "Everything" FAQ (USN Journal, indexing rates). https://www.voidtools.com/faq/
42. Hacker News, "Syncthing: Open Source Dropbox and BitTorrent Sync Replacement" (2014, 623 points). https://news.ycombinator.com/item?id=7734114
43. Hacker News, "Syncthing – a continuous file synchronization program" (2021, 545 points). https://news.ycombinator.com/item?id=28859521
44. LWN, "Syncthing: Syncing All the Things" (2021; HN thread 433 points). https://lwn.net/Articles/861978/ · https://news.ycombinator.com/item?id=27929194
45. Hacker News, "Syncthing Android App Discontinued" (2024, 418 points). https://news.ycombinator.com/item?id=41895718
46. Hacker News comment (ChronoSync tethered-photography workflow; sentiment). https://news.ycombinator.com/item?id=8308340
47. Hacker News comment (ChronoSync free updates; "VERIFY button"; sentiment). https://news.ycombinator.com/item?id=47225122

*Search/verification tooling: HN thread metadata via the Algolia HN Search API (`hn.algolia.com/api/v1/search?query=...`). DuckDuckGo HTML search was used for discovery of [4]/[5]; pages themselves were then fetched.*
