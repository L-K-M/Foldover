> Raw AI research report (opencode, GLM-5.3), 2026-10-16. Unedited. Corrections: [fact-checks](../README.md#corrections).

# macOS platform realities for a reliable, fast file-sync app

**Project:** Foldover — native macOS UI (Swift/SwiftUI/AppKit) over a portable (Rust) core; MVP = local & network sync (internal/external disks, SMB/NFS NAS, SFTP/SSH, Mac-to-Mac/LAN). Cloud providers out of MVP scope.
**Platform scope:** macOS 14 Sonoma → macOS 26 Tahoe, Apple Silicon. Forward-looking notes for macOS 27 ("Golden Gate") where relevant.
**Report date:** 2026-10-16. Every factual claim carries an inline source marker `[n]`; anything not confirmable in a primary source this round is explicitly marked **UNVERIFIED**.

---

## 1. Change detection

### 1.1 FSEvents fundamentals

FSEvents (Core Services, macOS 10.5+) is a three-part mechanism: "kernel code that passes raw event notification to user space through a special device (also used by spotlight), a daemon which filters this stream and sends out notifications, and a persistent database which stores a record of all changes throughout time" [2]. Key properties:

- **Granularity is a directory by default.** "It tells you *only* that something in the directory has changed, but does not tell you *what* changed" [2]. The daemon "coalesces multiple notifications on a given directory if they occur in a short period of time. You will always receive at least one notification after the last change is made" [2].
- **Latency is caller-chosen.** `FSEventStreamCreate(..., sinceWhen, latency, flags)` — "The number of seconds the service should wait after hearing about an event from the kernel before passing it along… Specifying a larger value may result in more effective temporal coalescing" [3]. Foldover should pick a small-but-nonzero latency (e.g. 0.3–1.0 s) to batch bursts.
- Apple explicitly positions FSEvents for "passively monitoring a large tree of files for changes… the foundation for Apple's backup technology," and says it is **not** a fine-grained, per-file, pre-emption mechanism; for watching a single file, "the kqueues mechanism is more appropriate" [2].

### 1.2 File-level events

- `kFSEventStreamCreateFlagFileEvents` (macOS 10.7+): "Request file-level notifications. Your stream will receive events about individual files… Use this flag with care as it will generate significantly more events" [4]. With this flag you get item-level flags (e.g. `kFSEventStreamEventFlagItemCreated / ItemRemoved / ItemRenamed / ItemModified / ItemInodeMetaMod / ItemXattrMod / ItemFinderInfoMod / ItemChangeOwner`, plus `ItemIsFile/ItemIsDir/ItemIsSymlink`) — these constants live in `FSEvents.h`/`FSEventStreamEventFlags` in Core Services. **UNVERIFIED online this round** (the individual constant pages weren't fetchable); verify against the SDK header.
- Other create-flags to use: `kFSEventStreamCreateFlagEventIds` (per-event IDs), `WatchRoot` (see 1.4), `IgnoreSelf` (suppress events caused by your own process), `UseCFTypes`, `NoDefer`. There is also `kFSEventStreamCreateFlagUseExtendedData` (macOS 10.13+, adds file IDs to events) — recommended for move detection, but **UNVERIFIED** here; check the header.
- Watch out: FSEvents historically had a **1024-character path limit** for watched roots (long paths are truncated/fail). Community-reported; **UNVERIFIED** — test explicitly.

### 1.3 Event IDs and `sinceWhen` replay

Event IDs are persistent per volume/host: store the last-seen ID and replay missed events after restart by passing it as `sinceWhen`; pass `kFSEventStreamEventIdSinceNow` to start "from now"; "Do not pass zero… unless you want to receive events for every directory modified since 'the beginning of time'" [3], [1]. Persistence details:

- Per-host vs per-disk streams: "if you are writing software that requires persistence, you should use per-disk streams" (`FSEventStreamCreateRelativeToDevice`) because per-host IDs can conflict when disks move between machines [1]. Per-device streams must validate the volume via `FSEventsCopyUUIDForDevice`; if the UUID changed (reformat, event-log purge) or the current ID is *lower* than your stored one (restore/wrap/purge), all stored events are invalid → full rescan [1].
- Apple's explicit warning: treat the event stream "as advisory rather than a definitive list of all changes… backup software should still periodically perform a full sweep of any volume to ensure that no changes fall through the cracks" [1]. The canonical pattern is events + a **cached directory-hierarchy snapshot** (names + metadata), diffing snapshot vs. disk on each event; start watching **before** scanning, and rescan any directory that changes during the scan [1].

### 1.4 Dropped events and forced rescans

- `kFSEventStreamEventFlagMustScanSubDirs` — events were coalesced across a subtree; "you must recursively rescan the path listed in the event" [1].
- `kFSEventStreamEventFlagKernelDropped` / `UserDropped` — communication loss between kernel and the daemon; "you must do a full scan of any directories that you are monitoring" (these flags are informational; `MustScanSubDirs` is also set) [1].
- `kFSEventStreamCreateFlagWatchRoot` + `kFSEventStreamEventFlagRootChanged` (event ID 0) — your watched root was moved/renamed/deleted; find its new path via `open()` + `fcntl(F_GETPATH)` and rescan [1].
- `kFSEventStreamEventFlagEventIdsWrapped` — 64-bit ID wrap; handle it [1].

### 1.5 FSEvents on external and network volumes

- **External (local) disks:** fully supported; use per-device streams keyed by the volume UUID (device IDs are not stable across reboots) [1].
- **Network volumes (SMB/NFS):** remote changes made by other clients *on the server* are not reported to your Mac — the kernel/fseventsd machinery only sees local volume events. This follows from the design (events are generated by the local kernel device and a per-volume persistent log [2], [1]) and is universal community wisdom for every macOS sync tool — but I could not find an authoritative Apple statement this round: **UNVERIFIED at primary source**. Design Foldover assuming **polling/rescan cycles for any network-mounted root**.
- **kqueue caveat:** "Not all filesystem types support kqueue-style notifications. And even some that do, like some remote filesystems, may only support a subset of the notification semantics" [5].

### 1.6 kqueue and dispatch sources

`kqueue(2)` `EVFILT_VNODE` watches a single file descriptor for `NOTE_DELETE, NOTE_WRITE, NOTE_EXTEND, NOTE_ATTRIB, NOTE_LINK, NOTE_RENAME, NOTE_REVOKE, NOTE_FUNLOCK` [5]; the same functionality is available as a GCD `DISPATCH_SOURCE_TYPE_VNODE` dispatch source ("monitors a file descriptor for events defined by Dispatch Source Vnode Event Flags") [6]. Use case for Foldover: watching a *specific* config/database file cheaply, exactly as Apple recommends for single files [2]. Don't build a tree watcher on kqueue — one descriptor per file does not scale to sync-sized trees.

### 1.7 Volume mount/unmount notifications

- **NSWorkspace** (AppKit): `NSWorkspace.didMountNotification`, `willUnmountNotification`, `didUnmountNotification`, `didRenameVolumeNotification` (plus `didWakeNotification` for wake) — posted on `NSWorkspace.notificationCenter` (not the default center); Swift concurrency wrapper `NSWorkspace.DidMountVolumeMessage` exists [7]. These cover all user-visible mounts **including network volumes**.
- **DiskArbitration** (macOS 10.4+): "Provides mechanisms to register and block disk mount or unmount events" — `DASessionCreate`, disk-appeared/disappeared/appeared callbacks, and the ability to *veto* unmounts via `DADissenter` [8]. Lower-level (IOKit-backed) and appropriate if Foldover must, e.g., hold a mount busy-free or inspect volume descriptions; NSWorkspace is enough for MVP mount detection.

### 1.8 When a full rescan is unavoidable (summary)

MustScanSubDirs / KernelDropped / UserDropped / RootChanged flags [1]; volume UUID or event-ID regression (restore, purge, reformat) [1]; app first run / folder added; watcher restart after error; wake from sleep if the event log can't be trusted (**UNVERIFIED** — treat post-sleep as "replay since stored ID, verify by snapshot diff, rescan on doubt"); *any* change detected on a network share (no server events) [1], [5]; macOS version upgrades (historically associated with watcher resets — community consensus, **UNVERIFIED**).

---

## 2. Filesystem semantics across APFS, HFS+, exFAT/FAT32, SMB, NFS

### 2.1 Behavior matrix

| Topic | APFS | HFS+ (Mac OS Extended) | exFAT / FAT32 | SMB (to NAS/Windows/Mac) | NFS |
|---|---|---|---|---|---|
| Default on macOS | Yes (10.13+) [11], [12] | Boot-supported legacy [12] | Interchange (exFAT >32 GB; FAT ≤32 GB) [12] | Apple's primary network FS since 10.9 (SMBX) [21] | UNIX shares; macOS supports NFSv2/v3 + NFSv4 over TCP [9] |
| Case sensitivity | Case-insensitive by default; "Case-sensitive" variant [12] | Case-insensitive by default; HFSX variant compares case-sensitively [10] | Case-insensitive | Server/NTFS semantics (case-preserving, case-insensitive typically) | Server-dependent (usually case-sensitive) |
| Name storage | Stores names as given; **normalization-insensitive** (**UNVERIFIED** this round; Apple's 2016 APFS Guide is offline) | "Stores strings fully decomposed and in canonical order" (NFD); case-insensitive comparison [10] | UTF-16, no normalization | Passes bytes; macOS client maps NTFS-illegal chars into Unicode private-use area [28], [29] | Passes bytes (server normalization rules apply) |
| Max name length | 255 UTF-8 bytes (**UNVERIFIED**) | 255 UTF-16 characters (`HFSUniStr255`) [10] | 255 UTF-16 chars (Microsoft spec; **UNVERIFIED**) | Server limits + mapped illegal chars [28], [29] | NAME_MAX (255 bytes typical) [15] |
| Timestamp granularity | Nanosecond fields (**UNVERIFIED**) | Seconds since 1904 (1 s) [10] | 2 s/10 ms fields (Microsoft spec; **UNVERIFIED**) | SMB/NTFS 100 ns units (**UNVERIFIED**) | Server FS |
| Birth (creation) time | Yes (`st_birthtime`) [15] | Yes [15] | Creation time exists | NTFS creation time (over SMB) | Generally absent → "set to 0 (i.e. epoch)" where unavailable [15] |
| xattrs | Native, large [16] | Native [16] | `._` AppleDouble sidecars [16] | Named streams/ADS when server supports [16], [17], [21] | Generally unsupported; NFSv4 optional client support [16] |
| Sparse files | Yes | No | No | Transferred **in full**, not re-sparse on return [21] | Server-dependent |
| Clones | Yes (`clonefile`) [13] | No | No | Not via clonefile (`ENOTSUP` off APFS) [13] | No |

### 2.2 Unicode normalization — the sync-equality trap

HFS+ converts every name to fully-decomposed NFD on disk [10]; APFS keeps whatever bytes you wrote but compares canonically (normalization-insensitive — **UNVERIFIED at a live Apple source**). Windows clients send NFC, Linux varies. Consequence for Foldover: **never compare filenames byte-for-byte across systems**. Normalize (NFC or NFD, pick one) for identity/hashing, keep raw bytes for round-trip fidelity, and treat NFD/NFC variants of one name as the same file when the volume itself does (HFS+/APFS) — otherwise you will create duplicate loops on Mac↔Linux sync. (General guidance; mechanism per [10].)

### 2.3 Illegal characters on SMB and Apple's SFM mapping

Windows/NTFS forbids `< > : " / \ | ? *` in names. When a Mac writes such a name to an SMB share, the macOS client "maps NTFS illegal characters to the Unicode private range in SMB requests" — e.g. `:` (0x3A) becomes **U+F03A** (0xF000 + char code), the classic "Services for Macintosh" (SFM) mapping [28], [29], with real-world examples [29]. Samba servers can un-map these (`fruit:encoding = native` + `vfs_catia`) [28]. Foldover must: detect private-use code points U+F000–U+F0FF in remote names and treat them as the corresponding ASCII chars; warn users; and never round-trip a mapped name back to a local volume expecting the same bytes [28], [29].

### 2.4 Timestamps and birth time

`st_birthtimespec` exists in the 64-bit-inode `struct stat`: "Time of file creation. Only set once… On filesystems where birthtime is not available, this field is set to 0 (i.e. epoch)" [15]. HFS+ stores dates as seconds since 1904 (1-second granularity, max 2040-02-06) [10]. Never rely on sub-second equality across filesystems; expect birth time to vanish on NFS/Linux targets [15]. Use mtime+size as the primary change signal and treat finer fields as advisory.

### 2.5 File IDs, hard links, and move detection

- HFS+ implements hard links through catalog "indirect node" files with a link reference number and link count [10]; APFS supports hard links natively (and per-file `st_ino` is stable across rename within a volume — standard UNIX behavior; **UNVERIFIED** explicitly this round, but see Howard Oakley's "Identify files in APFS using inode numbers", listed on his APFS index).
- For move detection: inode/file-ID + size + mtime is the robust triad; a rename within one volume preserves inode while path changes. `FSEventStreamCreateFlagUseExtendedData` reportedly surfaces file IDs in events (**UNVERIFIED** — verify in `FSEvents.h`).
- SMB servers may return unstable file IDs (Samba defaults `fruit:zero_file_id = yes` because its inode-based IDs "can therefore not be used" as HFS+ CNIDs by Mac clients) [28] — do not trust SMB file IDs for identity across reconnects; re-verify via path+size+mtime.
- Hard links over SMB: generally not portable; expect `link(2)` failures on shares. Represent hard-linked sets explicitly in your metadata DB, or flatten to copies (**design choice**).

### 2.6 Symlinks, aliases, bookmarks, packages

- Symlinks: portable across APFS/HFS+/Linux; on SMB servers store as-is only where supported (macOS↔macOS fine; NAS varies — **UNVERIFIED**). Never follow symlinks when hashing (use `lstat`, as Apple's own watcher sample pattern suggests [1]).
- Finder aliases/bookmarks are **not** portable — they embed volume/file IDs. For your own state (mounted roots, chosen folders) use `NSURL` bookmarkData (`bookmarkData(options:includingResourceValuesForKeys:relativeTo:)`) with `.withSecurityScope` under sandbox [34], and consider `.minimizing/bookmarkResolutionOptions` for volume moves. Aliases inside user trees should be synced as regular files (their data is a Finder artifact), not interpreted.
- Packages/bundles: the Finder treats bundle directories as single files via bundle bit/type codes [9]. Decide per-sync whether `.app`/packages are opaque files or trees; record the choice; preserve FinderInfo so the bundle bit survives (see §3).

### 2.7 Sparse files

APFS supports sparse files; copying a "sparse file of 1 GB nominal size… SMB will transfer the whole 1 GB to the share… and when that is copied back to local APFS… isn't reconstituted as an APFS sparse file" [21]. `copyfile(3)` offers `COPYFILE_DATA_SPARSE` ("Copy a file sparsely… requires that the source and destination file systems support sparse files…") with automatic fallback when combined with `COPYFILE_DATA` [14]. Foldover: track logical size vs. blocks (`st_blocks`) and never assume remote sparseness.

### 2.8 Clones: `clonefile()` / `copyfile(COPYFILE_CLONE)`

- `clonefile(2)` (10.12+) shares data blocks copy-on-write; same filesystem only (`EXDEV` otherwise), `ENOTSUP` where unsupported; test support per volume via `getattrlist` `ATTR_VOL_CAPABILITIES` → `VOL_CAP_INT_CLONE`; cloning directories is "strongly discouraged" (use `copyfile(3)` for trees); cloning a dataless file can return `EDEADLK` depending on I/O policy (`getiopolicy_np(3)`) [13].
- `copyfile(3)`: `COPYFILE_CLONE` (try clone, fall back to copy) and `COPYFILE_CLONE_FORCE` (clone or fail), both implying `EXCL|STAT|XATTR|DATA|NOFOLLOW_SRC`; ACLs only with `COPYFILE_ACL`; clones skip setuid/setgid bits; recursive clone per-entry; `COPYFILE_STATE_WAS_CLONED` tells you whether it cloned [14].
- Foldover should use `COPYFILE_CLONE | COPYFILE_ACL` (via the C API from the Rust core) for local APFS→APFS copies — near-instant, and preserves xattrs/FinderInfo — and plain data copies otherwise. No clone support over SMB [13].

### 2.9 Dataless files (iCloud evictions) and File Provider

- Since Sonoma, iCloud-evicted files are **dataless files**: the stub-file era (`.name.icloud`) ended; the file keeps all attributes/xattrs and logical size but has no extents; detect via `stat`/`getattrlist` — `SF_DATALESS` in `st_flags` [19] — or, for iCloud items, `URLResourceValues.ubiquitousItemDownloadingStatus` [19]. Apple published a developer Technical Note on dataless files in 2023 [19].
- Spotlight cannot see evicted (dataless) iCloud files until downloaded [31].
- Any read of a dataless file materializes it (downloads). `clonefile` of a dataless source may fail `EDEADLK` under restrictive I/O policy [13]. For a sync app this means: **knowingly download or skip** evicted files; offer "sync only downloaded items" modes; never stat-hash-loop blindly through iCloud Drive.
- File Provider framework (macOS 11+ `NSFileProviderReplicatedExtension`, older `NSFileProviderExtension`) is Apple's architecture for *remote-storage* sync — "If you just need to share local documents, see Share files locally" [44]; iCloud Drive migrated onto it in Sonoma 2023, most major cloud services followed [20]. It is **not** the right chassis for Foldover's local/NAS sync (see §6.2).

---

## 3. Metadata

### 3.1 The xattr landscape that matters to a sync app

- `com.apple.FinderInfo` (32-byte Finder info; bundle bit, custom icon, Finder lock) and `com.apple.ResourceFork` — both flagged persistent (`PCS` defaults) [18]; classic dual defined in `copyfile(3)`/AppleDouble handling [14].
- `com.apple.metadata:*` — Spotlight/Finder metadata; default flags `PS` (persistent, non-exportable); this family includes **Finder tags**, stored as `com.apple.metadata:_kMDItemUserTags` (binary plist of tag names) — the exact key is widely documented; **UNVERIFIED at a primary source this round**.
- `com.apple.quarantine` — Gatekeeper provenance of downloaded files; default flags `PCS`; Apple: Gatekeeper "verifies that the software is… notarized… and hasn't been altered" and "tracks the provenance of files written by downloaded software" [35], [18].
- `com.apple.provenance` — newer provenance-tracking xattr family added around macOS 13+ and refined since [18] (Oakley documents the family in "From quarantine to provenance…" and follow-ups; see his provenance series, e.g. "Quarantine, MACL and provenance", 2025-12-05, listed on his index).
- `com.apple.lastuseddate#PS` — example of a flag-suffixed xattr (the `#PS` is a property list, not part of the name) [18].

### 3.2 xattr flags and copy intents (the rules macOS itself follows)

Flags appended after `#`: `C` content-dependent, `P` no-export, `N` never-preserve, `S` syncable, `B` backup-only (macOS 14-era) [18]. Copy intents govern preservation: COPY preserves all but `N`/`B`; SAVE drops `C`; SHARE/EXPORT drops `P`; **SYNC (iCloud/FileProvider) keeps only `S`-flagged or non-`N`/`B` xattrs, and FileProvider additionally strips xattrs beyond ~32 KiB per item**; BACKUP keeps all but `N` [18]. Foldover's own state xattrs (if any) should be named with `#S` if they must survive iCloud, and kept small.

### 3.3 Limits

XATTR_MAXSIZE is 64 MiB in `xattr.h`; AppleDouble (`._`) sidecars are subject to a ~128 KiB per-xattr cap in practice [16]; xattr **names** are limited to 127 UTF-8 characters [18].

### 3.4 ACLs, BSD flags, ownership

- POSIX-style ACLs (macOS 10.4+ kernel ACLs) were added largely "to support network file systems such as SMB/CIFS"; evaluation order and inheritance (File/Directory Inherit, Inherit-Only, No-Propagate) are defined in the FS guide; SMB ACLs are enforced server-side only [9].
- BSD flags (`chflags(2)` / `st_flags`): `UF_NODUMP, UF_IMMUTABLE (uchg), UF_APPEND, UF_HIDDEN, SF_IMMUTABLE (schg), SF_ARCHIVED, SF_APPEND, UF_COMPRESSED` etc. [9], [15]; Finder's "Locked" is `UF_IMMUTABLE` — a sync engine must treat locked files as read-only and surface conflicts, and **must preserve UF_HIDDEN/SF_* on copy** via `COPYFILE_STAT`.
- Ownership: "Ignore ownership on this volume" (removable volumes) makes everything appear owned by the current user [9]; NFS maps root→nobody [9]; over SMB, enforcement is server-side [9].

### 3.5 The canonical copier: `copyfile(3)`

Use `copyfile`/`fcopyfile` with the right flags rather than hand-rolled copies: `COPYFILE_ACL | COPYFILE_STAT | COPYFILE_XATTR | COPYFILE_DATA` (= `COPYFILE_ALL`), plus `COPYFILE_RECURSIVE` for trees, `COPYFILE_NOFOLLOW_*` for links, `COPYFILE_EXCL`, `COPYFILE_MOVE`, `COPYFILE_PACK`/`UNPACK` for **AppleDouble serialization** ("Serialize the from file. The to file is an AppleDouble-format file") — useful for sidecar export of Mac metadata for non-Mac targets [14]. Progress callbacks (`COPYFILE_STATE_STATUS_CB`, `COPYFILE_STATE_COPIED`) exist; note "Recursive copies do not honor hard links" [14].

### 3.6 Metadata survival by target

| Target | What happens |
|---|---|
| APFS / HFS+ (same volume or Mac→Mac) | Full xattr/ACL/FinderInfo preservation; use clone or copyfile [14], [16] |
| exFAT / FAT32 | macOS writes hidden `._*` AppleDouble sidecars (~128 KiB/xattr cap) [16] — they pollute listings on other OSes and get lost if filtered |
| SMB to Mac/SMBX server | macOS stores xattrs as **named streams/ADS** ("later versions support what in Windows terms are alternate data streams (ADS), or named streams") [21]; enable/disable client-side via `nsmb.conf` `streams=yes` [17], [27]; broken server stream support ⇒ error −36/−50 on save [17] |
| SMB to Samba | With `vfs_fruit` + `vfs_streams_xattr`: streams in xattrs; FinderInfo/resource via Netatalk-compatible xattrs or `._` files (`fruit:metadata`, `fruit:resource`); without fruit: AppleDouble fallback and degraded enumeration [28] |
| SMB to plain Windows/NTFS server | NTFS ADS used for streams (server-dependent); NTFS "includes full support" for ADS [16] |
| NFS | "NFS as a general rule" does not support xattrs; macOS's NFSv4 client has an opt-in ("Enforce extended attributes and named forks if supported by server", `man 5 nfs`) [16] |
| Linux (SFTP/rsync target) | ext4/ZFS support xattrs; semantic mapping of FinderInfo/ACLs is yours to define [16] |

---

## 4. Network shares

### 4.1 The macOS SMB client (SMBX)

Apple replaced Samba with its own **SMBX** stack in OS X 10.7/10.9 and made SMB the primary protocol in 10.9 Mavericks (2013) [21]. AFP never worked with APFS volumes, which forced SMB for modern Macs [23]. Current state: "Apple's implementation of SMB has endured many bugs and problems over the years, but is now widely used and generally more stable… However, it doesn't support APFS special file types" (sparse/clone expansion, §2.7) [21].

### 4.2 Signing: defaults and cost

- History: macOS 10.9–10.13.3 defaulted to SMB2/3 packet signing **on** (a real performance drag); "In macOS 10.13.4 and later, packet signing is off by default. Packet signing for SMB 2 or SMB 3 connections turns on automatically when needed if the server offers it" [26] (Apple HT205926, archived copy). If a server *requires* signing, macOS pays the cost; disable per-network via `/etc/nsmb.conf` → `[default]` `signing_required=no` (client) [26], [27]; server side toggle `com.apple.smb.server SigningRequired` [26].
- Foldover-relevant: measure with signing on (default posture for security) and document the tunable for LAN users [26].

### 4.3 Multichannel

SMB Multichannel (macOS 11.3+, enabled by default) lets macOS open multiple connections for speed and redundancy; server must support it; macOS prefers the fastest advertised network; simultaneous use needs equal-speed interfaces; tunables `mc_on=no` (disable) and `mc_prefer_wired=yes` (Wi-Fi as failover only) in `/etc/nsmb.conf`; inspect live state with `smbutil multichannel` [25].

### 4.4 `nsmb.conf` and Apple's AAPL SMB2 extensions

- `nsmb.conf(5)` sections `[default]`, `[SERVER]`, `[SERVER:SHARE]`; documented keys include `addr, nbtimeout, minauth (kerberos|ntlmv2|ntlm|lm|none), port445, streams, soft, notify_off, smb_neg, signing_required`; user file at `~/Library/Preferences/nsmb.conf` is overridden by `/etc/nsmb.conf` [27] (man page reflects the 10.9-era client; newer SMBX keys like `mc_on`/`mc_prefer_wired` are documented by Apple support [25], and community-maintained lists add `dir_cache_off`, `protocol_vers_map`, etc. — community-sourced, **UNVERIFIED at primary source** [48]).
- **AAPL create context**: Mac clients negotiate Apple's SMB2+ "AAPL" extension. On Samba, `vfs_fruit`'s `fruit:aapl` (default yes) makes directory enumeration return Mac metadata (UNIX mode, FinderInfo, resource size, effective permissions) inline — "the Mac client doesn't need to fetch this metadata individually per directory entry resulting in an often tremendous performance increase" — and enables querying UNIX modes; also NFS-ACE support and an OS X copychunk ioctl (`fruit:copyfile`) [28]. Mixing fruit/non-fruit shares on one server disables AAPL globally [28]. Implication: a NAS without AAPL forces the Mac client into per-file metadata round-trips — expect slow first enumeration.
- Character mapping + metadata server-side config: `fruit:encoding = native|private` (SFM range, §2.3), `fruit:metadata = netatalk|stream`, `fruit:resource = file|xattr|stream` (`._` AppleDouble by default) [28].

### 4.5 `.DS_Store` and `._` files on shares

The Finder writes `.DS_Store` and macOS writes `._*` sidecars on network volumes (the `._` are your xattr lifeboat, §3.6 — don't blanket-ignore them if you sync Mac metadata). Classic client defaults to suppress `.DS_Store` on network volumes: `defaults write com.apple.desktopservices DSDontWriteNetworkStores true`; Apple has also documented preventing macOS from *reading* `.DS_Store` on SMB shares to speed browsing [49] (community gist quoting Apple's article; the original kbase URL is no longer live — **UNVERIFIED at primary source**). NAS admins often veto `._*`/`.DS_Store` server-side (`veto files=/._*/.DS_Store/`) [48] — which silently strips Mac metadata [28]. Foldover: default to skipping `.DS_Store` (configurable), never skipping `._` unless the user opts out.

### 4.6 Enumeration performance with many small files

Per-file metadata round-trips without AAPL (§4.4) plus one-by-one creation make large tree transfers crawl: a reader reports a Finder drag-and-drop of many small files over SMB being "at least an order of magnitude" slower than `scp`, and Oakley attributes it to "the inevitable overhead of transferring lots of small files one by one, with the requirement to create the file system metadata for them one by one"; Oakley also notes SMB "does still tend to choke on large numbers of small files" [21]. A notorious Sequoia 15.4 bug rendered folders with 1000+ files *empty/unreliably refreshed* over SMB; fixed in 15.5 [23] (user reports + Oakley's confirmation in comments). Design: pipeline directory enumeration, batch stat via AAPL-enriched FINDs where present, and keep an incremental index so first-sync cost is paid once.

### 4.7 Disconnects and reconnects

smbfs mounts drop on server sleep/network change; macOS auto-remounts "reconnectable" shares in Finder, but open FDs and your watcher state die. Treat network roots as unreliable: cache last-good index, detect via NSWorkspace unmount [7] + `NWPathMonitor` [42], resume with a scan-diff (no FSEvents on shares, §1.5). Soft mounts (`soft=yes` in nsmb.conf) change hang behavior on dead servers [27].

### 4.8 AFP status (2026)

Deprecated: Apple announced in the **macOS 15.5 Sequoia enterprise release notes (May 2025)** that a future macOS removes the AFP *client* [23]. Timeline: deprecation warning through macOS 26 Tahoe; **removed in macOS 27 Golden Gate** (released ~Sept 2026; mounting an AFP share now fails "Operation not supported") [24], [22]. Apple-silicon Macs long ago lost the AFP *server* while keeping the client [23]. Foldover: **no AFP support** — SMB (and NFS) only. Old Time Capsules (SMB1-only) are stranded [23].

### 4.9 NFS

macOS supports NFSv2/v3 (TCP/UDP) and NFSv4 over TCP [9]; NFS servers trust clients and map root→nobody [9]; xattr support is the exception, not the rule (§3.6) [16]. NFSv4.1 client support reportedly landed in macOS 26 (reported by a reader on Oakley's blog — **UNVERIFIED**) [21]. NFS remains a fine second protocol for Mac↔Linux/NAS sync of data forks, with metadata carried in your own sidecar format if needed.

### 4.10 Programmatic mounting, credentials, discovery

- Mount network URLs programmatically with the **NetFS** framework (`NetFSMountURLAsync`; supports `smb://`, `nfs://`, `afp://…`-style URLs, options dictionary, and keychain-backed auth) — reference: Apple docs "NetFS" framework, https://developer.apple.com/documentation/netfs — page not fetchable this round, so treat exact option-key names (`kNetFS*`) as **UNVERIFIED**; the header remains the source of truth.
- Credentials belong in the **login Keychain** (Security framework, `SecItemAdd`/`SecKeychain`-era APIs); never store passwords in your own DB.
- Discovery: **Bonjour** via `NWBrowser` (Network framework, macOS 10.15+) with a service descriptor for `_smb._tcp` etc.; `browseResultsChangedHandler` streams results [41]. Its docs reference the Info.plist keys `NSBonjourServices` and `NSLocalNetworkUsageDescription` [41] — browsing on the local network triggers the Local Network privacy consent prompt (macOS Sequoia+; timing **UNVERIFIED** this round). Ship both keys.

---

## 5. Distribution and permissions

### 5.1 Mac App Store path (sandbox) — poor fit for Foldover's MVP

- MAS requires App Sandbox ("To distribute a macOS app through the Mac App Store, you must enable the App Sandbox capability") [33], which fences you into entitlement-based access: user-selected files (`com.apple.security.files.user-selected.read-write`), downloads, pictures/music/movies folders — no blanket filesystem access; Full Disk Access "can't [be gained] automatically… The person using your app must choose to grant it" [33], [34].
- Persistence across launches is via **security-scoped bookmarks**: create with `.withSecurityScope`, resolve + `startAccessingSecurityScopedResource()`/`stop…`, recreate when stale [34] — workable, but every synced root must be user-picked, and helpers must inherit scopes via bookmark passing [34].
- Network mounting (NetFS) and login-item/daemon helpers from a sandboxed app are painful; a sync daemon effectively cannot run unsandboxed under MAS rules. Dropbox/Resilio-class sync tools ship Developer ID, not MAS.

**Recommendation:** Developer ID for MVP; revisit MAS only if a future feature subset fits the sandbox.

### 5.2 Developer ID + notarization + Sparkle

- Gatekeeper today: "all software in macOS is checked for known malicious content the first time it's opened," requiring App Store *or* Developer ID + notarization; hardened runtime, secure timestamps, no `get-task-allow`, `notarytool` submission and stapling; since 10.15 all Developer-ID-distributed software must be notarized [35], [36].
- Updates: **Sparkle 2** — "Secure. Updates are verified using EdDSA signatures and Apple Code Signing. Supports Sandboxed applications"; delta updates, appcast RSS, phased rollouts, channels; runtime macOS 12+ on 2.x [37].

### 5.3 TCC: Full Disk Access, Files & Folders, volume prompts

- Full Disk Access must be granted by the user in System Settings; there is no entitlement — build graceful failure and a setup flow that deep-links to the pane [34].
- Removable volumes: first un-consented access prompts; declare `NSRemovableVolumesUsageDescription`; reset for testing with `tccutil reset SystemPolicyRemovableVolumes <bundleID>` [39].
- Network volumes: same pattern with `NSNetworkVolumesUsageDescription` and `tccutil reset SystemPolicyNetworkVolumes <bundleID>` [40]. (Both keys are documented at macOS 10.15+ availability [39], [40].)

### 5.4 Background execution

- **Login item / agents / daemons:** `SMAppService` (macOS 13+) registers LoginItems, LaunchAgents, LaunchDaemons; `SMLoginItemSetEnabled` and `SMJobBless` are deprecated; status + `openSystemSettingsLoginItems()` for the approval UI [38]. Foldover MVP: main app + login item (user context). Avoid root daemons unless a feature demands it.
- **App Nap:** background apps get "Priority reduction… Timer throttling… I/O throttling"; naps are skipped if the app holds "IOKit power management or NSProcessInfo assertions" [46]. During active transfers, hold an `NSProcessInfo` activity (`beginActivity(options:.userInitiatedAllowingIdleSystemSleep…)`) — the assertion mechanism per [46]; exact `NSActivityOptions` names **UNVERIFIED** this round.
- **Preventing idle sleep mid-sync:** IOKit power assertions (`IOPMAssertionCreateWithName`, e.g. `kIOPMAssertionTypePreventUserIdleSystemSleep`) are the standard tool — see [46] for the assertion concept; the specific IOKit symbols page was not fetchable (**UNVERIFIED**).
- **Sleep/wake:** `NSWorkspace.willSleepNotification` / `didWakeNotification` [7]; on wake, replay FSEvents since stored ID (§1.3) and re-validate network mounts.
- **Low Power Mode:** exists on Mac laptops (Battery settings: Always / Only on Battery / Only on Power Adapter) [47]; detect via `NSProcessInfo.isLowPowerModeEnabled` (**UNVERIFIED** page) and throttle non-essential work.

### 5.5 Network awareness

`NWPathMonitor` + `NWPath`: `isExpensive` — "whether the path uses an interface that is considered expensive, such as Cellular or a Personal Hotspot" (macOS 10.14+) [42]; `isConstrained` similarly exists for Low Data Mode (sibling API, **UNVERIFIED** page). Pause/limit sync on expensive/constrained paths and on metered hotspots.

---

## 6. Integration points

### 6.1 FinderSync

The FinderSync extension ("Modify the Finder's user interface to express file synchronization and control") subclasses `FIFinderSync` to add **badges, contextual-menu items, and toolbar buttons** via `FIFinderSyncController` [43]. It is UI-only — the extension gets folder-watch plumbing for its own badge state, but your engine stays in the app/core. Status in 2026: still present and functional in current docs [43]; the API surface has been essentially frozen for a decade (no per-file richer overlays, no progress UI hooks) — community consensus, **UNVERIFIED**. Use it for status badges + "Share/Sync this folder" menu actions; ship as a separate, optional extension process.

### 6.2 File Provider — not for local/NAS sync

File Provider is for apps that "sync… with a remote storage" [44]; Apple explicitly separates local file sharing from File Provider ("If you just need to share local documents…" [44]). Domains live under the File Provider umbrella (`NSFileProviderManager`, `NSFileProviderDomain`) and historically surface content in the Finder's synced-location world; iCloud Drive moved onto it in Sonoma 2023 and major clouds followed [20]. For Foldover's local↔local/NAS sync, File Provider adds on-demand-placeholder machinery you don't need; revisit only for future cloud backends.

### 6.3 App Intents / Shortcuts

Expose start/pause sync, "sync this folder now", and status queries as `AppIntent`s (App Intents framework; `AppShortcutsProvider` for zero-setup shortcuts). Mechanism names are stable API surface; **UNVERIFIED** details this round (docs page not fetched).

### 6.4 MenuBarExtra

SwiftUI `MenuBarExtra` scene (macOS 13+) renders a persistent menu-bar control; supports `.menuBarExtraStyle(.window)` popover-style panels; menu-bar-only utility apps should set `LSUIElement = true` [45]. Ideal for Foldover's status/queue UI.

### 6.5 UserNotifications, WidgetKit

Use `UNUserNotificationCenter` (UserNotifications) for conflict/completion alerts and WidgetKit for a status widget (notification-center/desktop). Standard frameworks; **details UNVERIFIED this round** — trivial to confirm in Xcode docs.

### 6.6 Spotlight

- Network SMB shares are effectively unsearchable by local Spotlight in Sequoia: "Shared volumes and folders using SMB are unlikely to be accessible to local Spotlight search… there seems to be no reliable way of enabling this" [31]; server-side Spotlight (Samba + Elasticsearch backend) works when the server implements it [32] (per Samba wiki, referenced in [31]'s comments).
- Evicted iCloud (dataless) files are invisible to Spotlight until downloaded [31].
- Exclusion controls: folder suffix `.noindex` still works; hidden-dot names work; `.metadata_never_index` file "no longer works in recent macOS"; System Settings → Spotlight Privacy; re-index with `mdutil -E <volume>` [31]. Foldover's own index/cache directory should be `.noindex`-suffixed (and `.nosync`-style where applicable — the `.nosync` suffix is iCloud folklore, **UNVERIFIED**).

### 6.7 Quick Look

Rely on the system's QL generators for previews; for custom bundle types register a QL generator / use `QLThumbnailGenerator` (QuickLookThumbnailing) — advice-level; no time-sensitive claims.

### 6.8 Time Machine exclusions

API-level exclusion is `CSBackupSetItemExcluded` (CoreServices) / `NSURLIsExcludedFromBackupKey` — **UNVERIFIED** this round (reference pages unreachable). Do exclude Foldover's internal cache/DB from TM to avoid backing up transient sync state (and consider `.noindex` too, §6.6).

---

## 7. Gotchas checklist for Foldover

1. **FSEvents is advisory.** Always keep a snapshot DB + full periodic sweep; Apple says so explicitly. *Handling:* snapshot-diff engine; schedule full scans. [1]
2. **Handle `MustScanSubDirs` / `KernelDropped` / `UserDropped` / `RootChanged`** — each means "rescan now". *Handling:* map every flag to a rescan scope in the core. [1]
3. **Persist `sinceWhen` per volume, keyed by volume UUID** (`FSEventsCopyUUIDForDevice`); lower-ID-than-stored ⇒ treat as full reset. [1]
4. **Use per-device streams** for durable watchers; per-host streams can collide when disks move. [1]
5. **Never trust FSEvents on network mounts.** Poll + diff on an adaptive interval; mount/unmount via NSWorkspace to start/stop. [1], [5], [7]
6. **Latency ≠ 0.** Pick a small latency to coalesce bursts; larger latency = fewer callbacks. [3]
7. **kqueue/`DISPATCH_SOURCE_TYPE_VNODE` only for single files** (config, DB); remote filesystems may support only subsets. [2], [5], [6]
8. **Unicode normalization:** HFS+ stores NFD [10]; normalize for identity, preserve raw bytes on write; expect NFC from Windows/Linux peers. Beware APFS normalization-insensitivity (**UNVERIFIED** primary source).
9. **SFM character mapping on SMB:** NTFS-illegal chars appear as U+F0xx private-use on shares; normalize/warn; Samba can unmapping with `fruit:encoding=native` [28], [29].
10. **Timestamp granularity mismatch:** HFS+ = seconds [10]; birth time may be 0 on some FS [15]. Compare with tolerance (e.g. ±2 s) and never use ctime as truth.
11. **Sparse files inflate over SMB** (1 GB hole = 1 GB stored remotely, not re-sparse on return). Track `st_blocks`; document behavior. [21], [14]
12. **Clones:** use `copyfile(COPYFILE_CLONE)` for local APFS copies; `clonefile` fails `EXDEV` cross-volume, `ENOTSUP` on non-APFS; ACLs need `COPYFILE_ACL`; dataless sources can `EDEADLK`. [13], [14]
13. **Dataless (iCloud) files:** check `SF_DATALESS` before hashing; reading materializes/downloads; make behavior explicit. [19], [13]
14. **xattr copying is rule-bound:** sync-intent strips non-`S` xattrs (iCloud) and >32 KiB xattrs; your own xattrs need `#S` and small size; `._` sidecars on exFAT cap ~128 KiB. [18], [16]
15. **`com.apple.FinderInfo`/`ResourceFork` carry the bundle bit and Finder state** — losing them breaks packages/custom icons; use `COPYFILE_METADATA` semantics. [18], [14]
16. **ACLs enforced server-side on SMB; root→nobody on NFS; "ignore ownership" on remounted drives.** Don't replicate raw ACLs blindly; model effective permissions. [9]
17. **SMB signing can silently throttle** when the server requires it; `nsmb.conf signing_required=no` is the escape hatch (secure-network caveat). [26]
18. **Multichannel needs same-speed interfaces for speedup;** `mc_prefer_wired=yes` keeps Wi-Fi as failover. [25]
19. **AAPL-less NAS ⇒ per-file metadata round-trips ⇒ glacial first sync.** Recommend fruit-enabled Samba or Mac servers; batch metadata reads. [28]
20. **Many-small-files trees are pathological over SMB** (order-of-magnitude slowdowns reported); consider bundling/parallel pipelines; Sequoia 15.4's empty-folder SMB bug (fixed in 15.5) is a reminder to pin minimum macOS versions. [21], [23]
21. **`._` files are your metadata on exFAT/Netatalk-style setups; `.DS_Store` is Finder noise.** Skip `.DS_Store` by default; never skip `._` unless opted in. [16], [28], [49]
22. **Spotlight can't see SMB shares (and evicted iCloud files).** Set expectations; don't promise search over NAS mirrors. [31]
23. **AFP is dead (deprecated 15.5, removed in macOS 27).** SMB/NFS only; detect AFP-only servers (Time Capsules) and explain. [23], [24], [22]
24. **Sandbox (MAS) makes daemon-style sync impractical** — user-picked roots, security-scoped bookmarks, no silent install-time access. Choose Developer ID + notarization + Sparkle 2. [33], [34], [36], [37]
25. **TCC prompts:** Full Disk Access is manual; removable + network volume prompts need usage strings (`NSRemovableVolumesUsageDescription`, `NSNetworkVolumesUsageDescription`); provide `tccutil reset` instructions for QA. [39], [40], [34]
26. **Register login item via `SMAppService`** (13+); legacy APIs deprecated. [38]
27. **App Nap throttles timers/IO in background**; hold NSProcessInfo/IOKit assertions during active work; respect Low Power Mode. [46], [47]
28. **Bonjour browsing triggers the Local Network prompt** — ship `NSLocalNetworkUsageDescription` + `NSBonjourServices`. [41]
29. **Pause on expensive/constrained networks** via NWPathMonitor (`isExpensive`). [42]
30. **Exclude internal state from Time Machine and Spotlight** (`CSBackupSetItemExcluded`/`NSURLIsExcludedFromBackupKey` — **UNVERIFIED**; `.noindex` suffix for Spotlight works). [31]

---

## Sources

1. Apple, *File System Events Programming Guide — Using the File System Events API* (Documentation Archive, updated 2012-12-13): https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/UsingtheFSEventsFramework/UsingtheFSEventsFramework.html
2. Apple, *File System Events Programming Guide — Technology Overview* (Documentation Archive): https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/TechnologyOverview/TechnologyOverview.html
3. Apple, *FSEventStreamCreate* (developer reference): https://developer.apple.com/documentation/coreservices/1443980-fseventstreamcreate
4. Apple, *kFSEventStreamCreateFlagFileEvents* (developer reference): https://developer.apple.com/documentation/coreservices/kfseventstreamcreateflagfileevents
5. macOS man page, *kqueue(2)*: https://keith.github.io/xcode-man-pages/kqueue.2.html
6. Apple, *DISPATCH_SOURCE_TYPE_VNODE*: https://developer.apple.com/documentation/dispatch/dispatch_source_type_vnode
7. Apple, *NSWorkspace.didMountNotification* (see-also lists unmount/rename/wake notifications; `DidMountVolumeMessage`): https://developer.apple.com/documentation/appkit/nsworkspace/didmountnotification
8. Apple, *DiskArbitration framework*: https://developer.apple.com/documentation/diskarbitration
9. Apple, *File System Programming Guide — File System Details* (Documentation Archive, updated 2018-04-09): https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemDetails/FileSystemDetails.html
10. Apple, *Technical Note TN1150: HFS Plus Volume Format* (2004-03-05): https://developer.apple.com/legacy/library/technotes/tn/tn1150.html
11. Apple Platform Security, *Role of Apple File System* (published 2024-12-19): https://support.apple.com/guide/security/role-of-apple-file-system-seca6147599e/web
12. Apple, Disk Utility User Guide, *File system formats available in Disk Utility on Mac*: https://support.apple.com/guide/disk-utility/file-system-formats-dsku19ed921c/mac
13. macOS man page, *clonefile(2)*: https://keith.github.io/xcode-man-pages/clonefile.2.html
14. macOS man page, *copyfile(3)*: https://keith.github.io/xcode-man-pages/copyfile.3.html
15. macOS man page, *stat(2)* (st_birthtime, st_flags, ENAMETOOLONG/NAME_MAX): https://keith.github.io/xcode-man-pages/stat.2.html
16. Howard Oakley, *Which file systems and Cloud services preserve extended attributes?* (2018-01-12): https://eclecticlight.co/2018/01/12/which-file-systems-and-cloud-services-preserve-extended-attributes/
17. Howard Oakley, *Q&A: Samba's named streams* (2015-08-27): https://eclecticlight.co/2015/08/27/qa-sambas-named-streams/
18. Howard Oakley, *From quarantine to provenance: how xattrs are copied* (2024-09-13): https://eclecticlight.co/2024/09/13/from-quarantine-to-provenance-how-xattrs-are-copied/
19. Howard Oakley, *Last Week on My Mac: Dataless files arrive* (2023-10-29): https://eclecticlight.co/2023/10/29/last-week-on-my-mac-dataless-files-arrive/
20. Howard Oakley, *Explainer: File Provider and cloud services* (2026-05-09): https://eclecticlight.co/2026/05/09/explainer-file-provider-and-cloud-services/
21. Howard Oakley, *Explainer: Network file systems* (2026-04-25, incl. reader reports on small-file SMB performance): https://eclecticlight.co/2026/04/25/explainer-network-file-systems/
22. Howard Oakley, *Networking changes coming in macOS 27* (2026-04-23): https://eclecticlight.co/2026/04/23/networking-changes-coming-in-macos-27/
23. Howard Oakley, *Check your network backups and shares, as AFP is being removed* (2025-05-15, incl. Sequoia 15.4/15.5 SMB bug reports in comments): https://eclecticlight.co/2025/05/15/check-your-network-backups-and-shares-as-afp-is-being-removed/
24. ELEMENTS (Syslink GmbH), *Apple Removes AFP Support in macOS 27 Golden Gate* (2026-09-18): https://elements.tv/blog/apple-removes-afp-support-in-macos-27-golden-gate/
25. Apple Support, *Configure SMB Multichannel behavior* (published 2025-03-21): https://support.apple.com/en-us/102010
26. Dave Nathanson, *Fix SMB connection problems for Mac* (2019-01, quoting Apple Support HT205926 "Turn off packet signing for SMB 2 and SMB 3 connections"): https://www.nathanson.org/davesays/2019/fix-smb-connection-problems-for-mac/
27. macOS man page, *nsmb.conf(5)* (via manpagez; macOS 10.9-era text): https://www.manpagez.com/man/5/nsmb.conf/
28. Samba, *vfs_fruit(8)* man page (AAPL extension, streams, encoding, zero_file_id): https://www.samba.org/samba/docs/current/man-html/vfs_fruit.8.html
29. Libation issue #1256, *Export to SMB share on macOS mangles Windows-illegal characters into U+F0xx* (observed `:` → U+F03A): https://github.com/rmcrackan/Libation/issues/1256 ; see also Qumolo, *Translate Illegal Characters over SMB*: https://care.qumulo.com/s/article/Translate-Illegal-Characters-over-SMB
30. Apple, *NSRemovableVolumesUsageDescription*: https://developer.apple.com/documentation/bundleresources/information-property-list/nsremovablevolumesusagedescription (numbered 39 below — see index)
31. Howard Oakley, *Using and troubleshooting Spotlight in Sequoia: summary* (2024-11-29; SMB/dataless search limits, exclusions, mdutil): https://eclecticlight.co/2024/11/29/using-and-troubleshooting-spotlight-in-sequoia-summary/
32. Samba wiki, *Spotlight with Elasticsearch Backend* (server-side Spotlight; referenced in [31] comments): https://wiki.samba.org/index.php/Spotlight_with_Elasticsearch_Backend
33. Apple, *App Sandbox*: https://developer.apple.com/documentation/security/app-sandbox
34. Apple, *Accessing files from the macOS App Sandbox* (security-scoped bookmarks, FDA): https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox
35. Apple Platform Security, *Gatekeeper and runtime protection in macOS* (published 2024-12-19): https://support.apple.com/guide/security/gatekeeper-and-runtime-protection-sec5599b66df/web
36. Apple, *Notarizing macOS software before distribution*: https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution
37. Sparkle project, *Sparkle 2 README* (EdDSA, sandbox support, macOS 12+ on 2.x): https://github.com/sparkle-project/Sparkle
38. Apple, *SMAppService*: https://developer.apple.com/documentation/servicemanagement/smappservice
39. Apple, *NSRemovableVolumesUsageDescription*: https://developer.apple.com/documentation/bundleresources/information-property-list/nsremovablevolumesusagedescription
40. Apple, *NSNetworkVolumesUsageDescription*: https://developer.apple.com/documentation/bundleresources/information-property-list/nsnetworkvolumesusagedescription
41. Apple, *NWBrowser* (macOS 10.15+; NSBonjourServices / NSLocalNetworkUsageDescription): https://developer.apple.com/documentation/network/nwbrowser
42. Apple, *NWPath.isExpensive* (macOS 10.14+): https://developer.apple.com/documentation/network/nwpath/isexpensive
43. Apple, *FinderSync framework*: https://developer.apple.com/documentation/findersync
44. Apple, *FileProvider framework* (replicated/nonreplicated extensions; "Share files locally"): https://developer.apple.com/documentation/fileprovider
45. Apple, *MenuBarExtra* (SwiftUI, macOS 13+; LSUIElement; .window style): https://developer.apple.com/documentation/swiftui/menubarextra
46. Apple, *Energy Efficiency Guide for Mac Apps — Extend App Nap* (Documentation Archive, updated 2016): https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/AppNap.html
47. Apple Support, *Save energy on your Mac* (Mac User Guide; Low Power Mode options): https://support.apple.com/guide/mac-help/optimize-your-mac-battery-life-mh35848/mac
48. Community gist (jakkuh), *MacOS nsmb.conf performance fixes* (last updated 2025-07; community-sourced keys): https://gist.github.com/jakkuh/cd4c4d18e819f6f660679df98818ab36
49. Community gist (jbfriedrich), *macOS 11.2 NSMB configuration* (quoting Apple on .DS_Store reading over SMB; DSDontWriteNetworkStores): https://gist.github.com/jbfriedrich/49b186473486ac72c4fe194af01288be

*Note on UNVERIFIED items:* APFS normalization-insensitivity & name-length specifics, FSEvents item-flag/extended-data constant pages, 1024-char watch-path limit, exFAT/FAT & SMB timestamp granularities, NetFS option keys, `NSProcessInfo` activity/Low-Power API pages, IOKit `IOPMAssertionCreateWithName`, `CSBackupSetItemExcluded`/`NSURLIsExcludedFromBackupKey`, macOS 26 NFSv4.1 support, FinderSync API freeze, App Intents/QuickLook/UserNotifications details, and the macOS Sequoia Local-Network-prompt introduction date could not be confirmed at a primary source during this pass (several Apple reference pages are JS-only or moved); they are flagged inline and should be validated against SDK headers/Xcode docs before relying on them.
