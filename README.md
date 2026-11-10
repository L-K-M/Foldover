# Foldover

> [!IMPORTANT]
> LLM disclosure: This codebase was written with substantial help from large language models: AI coding agents working from the [`AGENTS.md`](AGENTS.md) brief in this repo.

**Source version:** v<!-- version -->0.2.0<!-- /version --> · [Releases](https://github.com/L-K-M/Foldover/releases)

[![CI](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml/badge.svg)](https://github.com/L-K-M/Foldover/actions/workflows/ci.yml)

Sync folders between your Mac's disks, NAS shares, SFTP servers and other
Macs, and see exactly what will happen before it happens. No cloud, no
account, no subscription.

Free and open-source under the Unlicense. No sponsorships or paid features.
The macOS app requires macOS 27 and Apple silicon.

> [!NOTE]
> **Status: beta in preparation.** Folders on this Mac, SFTP servers and other
> Macs running Foldover sync from the app and the command line. Do not trust it
> with data you have not backed up. [`PLAN.md`](PLAN.md) is the
> source of truth for scope and design; the research behind it lives in
> [`docs/research`](docs/research/README.md).

## Guides

- [Using the app](docs/guide/using-the-app.md): the window, menus,
  keyboard shortcuts, Undo and the archive.
- [What each folder kind can do](docs/guide/capabilities.md): local
  folders, mounted shares and SFTP folders side by side.
- [NAS shares and SFTP servers](docs/guide/nas-and-sftp.md): mounting,
  Keychain credentials, reconnect and SSH setup.
- [Limitations](docs/guide/limitations.md): current limits and what is
  not implemented yet.

## Highlights

- **Preview first.** Every change is listed before it runs, with per-item
  overrides.
- **Nothing lost quietly.** Replaced and deleted files go to an archive;
  conflicts wait for you instead of being silently resolved.
- **Mac to Mac, built in.** Other Macs appear on the network; pair with a
  code, and sync encrypted over the LAN.
- **Native and fast.** A SwiftUI app over a portable Rust engine that keeps
  syncing with the window closed. A Linux app follows.

## Install

Download the latest `Foldover-X.Y.Z.zip` from
[Releases](https://github.com/L-K-M/Foldover/releases) (macOS 27, Apple
silicon). The app is signed ad hoc, not notarized: after moving it to
`/Applications`, run `xattr -dr com.apple.quarantine /Applications/Foldover.app`
once, then open it.

## Building

```
scripts/build.sh            # everything this host can build, staged in dist/
scripts/build.sh core       # Rust agent and CLI (macOS or Linux)
scripts/build.sh app        # macOS app (macOS 27+, Xcode 27+, XcodeGen)
scripts/build.sh --install  # also install the app into /Applications
scripts/build.sh app --install --no-open  # install without opening GUI windows
cargo xtask ci              # the Linux CI gate
```

Toolchain details and every other command: [`AGENTS.md`](AGENTS.md).

## Background agent

The app embeds the Rust agent and CLI. Choose **Turn On** to start the
background agent; if macOS requests approval, open Login Items and allow
Foldover, then refresh.
The agent configures and syncs pairs, and keeps syncing with the window closed.
Choose **New Pair**, select both folders and a mode, then save. Select the pair
and choose **Preview** to see every change, then **Sync Now**. A pair with a
schedule can start its first sync by itself after you save it; turn on
**Require approval before syncing** to make every sync wait for you. The
menu bar shows each pair's status.

For development, run these in separate terminals:

```sh
cargo run -p foldoverd
cargo run -p foldover-cli -- status
```

`cargo xtask agent-smoke` tests the built agent and CLI in a private runtime
directory, including single-instance locking, malformed requests, crash
restart and shutdown. It also runs inside `cargo xtask ci`.

### Pair configuration

On macOS, use the installed app's bundled CLI. Standalone development builds
can check health but cannot access private configuration. Linux checks the
connecting user ID.

```sh
"/Applications/Foldover.app/Contents/Library/LaunchServices/foldover" pairs
"/Applications/Foldover.app/Contents/Library/LaunchServices/foldover" pairs add \
  "Photos backup" "/Users/you/Pictures" "/Volumes/Backup/Photos" backup-left \
  --permissions sync --durability full
```

A folder may also be on an SFTP server, written
`sftp://[user@]host[:port]/path` (`/~/…` for the home folder). Foldover
connects with your SSH keys, agent and `~/.ssh/config`, and never asks for
a password ([ADR 0028](docs/adr/0028-sftp-endpoints.md)).

Both selected folders must exist. Roots must be disjoint from each other
and every configured pair, and may not be or sit inside Foldover's state
directory — but a folder that *holds* it, such as your home folder, syncs
fine: the state directory is left out of scans, refused to reads and
writes, and never shared with a paired Mac. Reconnect unavailable
configured roots before adding or changing pairs, so overlap checks can finish.
Configuration does not scan contents or authorize sync execution.

Modes: `sync`, `mirror-left`, `mirror-right`, `backup-left`, `backup-right`.
The suffix selects the source. Options follow the mode:
`--times`, `--attributes` and `--permissions` take `sync` or `ignore`
(defaults: sync, sync, ignore); `--durability` takes `standard` (default) or
`full`, which also survives power loss but is slower. `--records source`
lets a `backup-*` target that cannot hold `.foldover` keep nothing: files
replaced there are deleted, not archived, and Undo is unavailable
(default `both`). `--schedule` takes
`manual` (default), `changes` (sync shortly after files change, and an hour
after the last preview or sync ends), a number of minutes between automatic
syncs, or `daily@HH:MM[:DAYS]` for a local time of day, with days as a
comma list or range such as `mon-fri` (every day when omitted). A scheduled
pair's first sync can start by itself. `--exclude PATTERN` (repeatable) sets
gitignore-style rules for files to skip in both folders; a `.foldoverignore`
file in either folder adds more. `--max-size MEGABYTES` skips larger files in
both folders and `--max-age DAYS` files unchanged for longer in both (`none`
removes either limit). `--bandwidth-limit MEGABYTES` caps the pair's copying
at that many megabytes per second (`none` removes it;
[ADR 0044](docs/adr/0044-bandwidth-limit.md)). `--canary PATH` names a file
inside the folders that must be present and unchanged in both before a sync
writes anything (`none` removes it;
[ADR 0033](docs/adr/0033-canary-file.md)). `--review required` makes every
sync, scheduled ones included, stop until you approve its plan (default
`automatic`). `--removal-protection disabled` turns off the stops for
unusually large removals and for a suddenly empty folder (default
`enabled`; [ADR 0020](docs/adr/0020-run-execution.md)). Resource forks are content; SFTP copies fail when they cannot
preserve them. Changing roots or metadata options resets the pair's sync history.
`pairs show ID` reads one pair;
`pairs update ID REVISION NAME LEFT RIGHT MODE [OPTIONS]` requires its current
revision and keeps options you do not name.
`pairs remove ID REVISION` marks the pair removed, including for an offline
pair; the pair disappears at once from every list and lookup, and the agent
deletes its rows in the background. Your folders stay in place. The app
offers the same **Remove Pair** action. Commands output JSON lines with exact
root-byte arrays. Lists are paged by identity and are not an atomic catalog
snapshot. Refresh after concurrent edits. If a command times out, inspect the
catalog before retrying: a disconnected client does not cancel a committing
configuration change. `pairs remove` answers at once; it does not wait for
the background delete.

### Sync

```sh
foldover --help                   # usage; exits successfully
foldover sync ID --preview       # list changes; writes nothing
foldover sync ID --changes       # every preview item, across all pages
foldover sync ID --changes conflicts  # only conflicts in the held preview
foldover sync ID --progress      # distinct working statuses, then the result
foldover sync --all              # every pair, at most four at once
foldover sync ID                 # run; routine approval is off by default
foldover sync ID --approve 60 0 --review TOKEN  # approve the reviewed deletions
foldover sync ID --keep-right PATH --review TOKEN  # keep B's reviewed version
foldover sync ID --keep-both PATH --review TOKEN   # keep both reviewed versions
foldover sync ID --skip PATH --review TOKEN        # leave a reviewed change for one run
foldover sync ID --undo          # undo the newest sync that changed anything
foldover history ID              # finished runs, newest first
foldover pause [MINUTES]         # hold automatic syncs; `foldover resume` ends it
foldover pairs pause|resume ID   # hold one pair's automatic syncs, or end it
foldover automation effort full  # automatic syncs at full speed; `gentle` is the default
foldover safe-mode off           # sync again after the agent kept crashing
foldover archive ID left         # archived runs in folder A, newest first
foldover archive ID left RUN     # items one run archived
foldover archive ID left RUN --put-back PATH
foldover archive ID left RUN --put-back-all
```

Each command waits for its job and prints the final status as JSON.
`--progress` prints working statuses as JSON lines too. `--changes` reuses
the held preview, or previews first; its optional kind is `copies`, `updates`,
`archives`, `conflicts`, `blocked`, `kept` or `moves`. Sync All prints one
JSON line per pair as it finishes. Its exit code gives errors precedence
over conflicts, then approval, then success.
Place `--progress` after the sync options and before `--socket PATH`;
path and option values remain literal.

A run that finishes with unresolved conflicts and no failed items exits with
status 4; failures exit with status 1 even when conflicts also remain,
and invalid command usage with status 2. A partial
`--put-back-all` also exits with status 1 and prints its full restore report;
failed items remain archived. Clean runs exit with status 0. A run
stops before writing, exiting with status 3, when the pair requires
approval (`--review required`), when it would delete more than the
automatic limits allow, or when one folder is suddenly empty (the last two
unless `--removal-protection disabled`); rerun with `--approve` and the preview's
`review_token` as `--review TOKEN`. Choices also echo that token. A changed plan
or configuration requires a fresh review. Replaced and removed
files go to `.foldover/archive/<run>/` inside the folder where they were
for 30 days. Put back returns one to its path, or beside it as
`name (restored).ext` when that path is taken; the next sync copies it
across ([ADR 0025](docs/adr/0025-archive-browser.md)).
The first sync writes `.foldover/id` into both folders; later runs stop if
either is missing or different, for example when a disk is not mounted.

The API contract and remaining gates are in
[ADR 0011](docs/adr/0011-pair-configuration-api.md).

## Opening a downloaded build

Builds are signed ad hoc, without Developer ID or notarization. Extract the
download, move `Foldover.app` to `/Applications`, then run:

```sh
xattr -dr com.apple.quarantine "/Applications/Foldover.app"
open "/Applications/Foldover.app"
```

This removes quarantine only from Foldover and its embedded code. File
access requires separate permissions.

## License

Foldover's own code is released into the public domain under the
[Unlicense](LICENSE). The app and its embedded `foldoverd` and
`foldover` helpers bundle third-party Rust crates and the Rust standard
library, which keep their own licenses: mostly MIT and Apache-2.0, also
ISC, BSD-3-Clause, Unicode-3.0, and MPL-2.0 for UniFFI. Their notices
ship in the app's `Contents/Resources` as `Third-Party-Notices.txt` and
`Rust-Licenses.html`. The UniFFI crates are used unmodified; the
notices file gives the location of their source. `foldoverd` also
includes SQLite, which is in the public domain.

The Unlicense covers Foldover's own code only. The generated Swift
bindings in `apple/FoldoverCore/Sources` and `apple/FoldoverCore/Headers`
contain code from UniFFI's templates, which keeps its MPL-2.0 license.
`licenses/` holds third-party license texts for the notices, not
Foldover's license.
