# NAS shares and SFTP servers

Two ways to sync with storage that is not directly attached to your Mac:

- **Network share**: mount it in macOS, then pick the folder under
  `/Volumes` like any local folder. Works with SMB, NFS and WebDAV;
  macOS 27 has no AFP client.
- **SFTP folder**: name a folder on a server with an `sftp://` locator.

## A network share

### Find a server

The pair editor lists the SMB servers Bonjour finds on your local
network under "Servers nearby", so you do not need to know an address.
Connect there opens the server in Finder's Connect to Server; choose a
shared folder, then pick it under `/Volumes` as below. The same list
prints as JSON lines with `foldover servers`.

### Set it up

1. Mount the share in Finder: Go > Connect to Server, for example
   `smb://nas.local/Photos` or `nfs://nas/export/media`. Turn on
   "Remember this password in my keychain" when macOS asks. The Keychain
   entry is what Foldover uses to reconnect later; a share whose
   credentials are not saved stays missing
   ([ADR 0027](../adr/0027-share-reconnect.md)).
2. In Foldover, choose the folder under `/Volumes/<share>` for one or
   both sides of the pair.
3. On each run Foldover records where that volume is mounted from, such
   as `smb://alice@nas.local/Photos`, `nfs://nas/export/media` or a
   WebDAV URL
   ([ADR 0027](../adr/0027-share-reconnect.md)).

### When the share is not mounted

- A scheduled run tries to mount the share again through macOS NetFS,
  with no dialog, using your Keychain credentials. It tries at most once
  every five minutes per pair and otherwise waits
  ([ADR 0027](../adr/0027-share-reconnect.md)).
- A manual run or preview tries once, then fails and names the missing
  side ([ADR 0027](../adr/0027-share-reconnect.md),
  [ADR 0022](../adr/0022-scheduled-runs.md)).
- While a scheduled pair waits its status shows `waiting`; it produces
  no notification and no history entry, and it runs at the first check
  after the folder is back ([ADR 0022](../adr/0022-scheduled-runs.md)).
- A share mounted at a different path, such as `/Volumes/Photos-1`,
  still counts as missing. After a reconnect the folder's `.foldover/id`
  file confirms it is the same folder before anything is written
  ([ADR 0027](../adr/0027-share-reconnect.md),
  [ADR 0020](../adr/0020-run-execution.md)).
- If the Keychain holds no usable credentials, the share stays missing.
  Mount it once in Finder and save the password
  ([ADR 0027](../adr/0027-share-reconnect.md)).

### Server settings

Foldover has no special Samba or NFS server requirements beyond what the
ADRs state: macOS mounts the share and your credentials live in the
Keychain. What a real SMB or NFS server preserves of macOS metadata has
not been measured; treat tags, comments, resource forks, ACLs and
name-handling fidelity as unverified
([ADR 0005](../adr/0005-metadata-fidelity.md),
[ADR 0027](../adr/0027-share-reconnect.md)).

macOS writes AppleDouble files (`._*`) beside files on volumes that cannot
store its metadata, and these stop a scan. Add a `._*` rule to the pair's
filters to skip them ([limitations](limitations.md),
[ADR 0024](../adr/0024-filters.md)).

## An SFTP folder

### Set it up

1. Make key-based SSH work first. `ssh user@host` must succeed without a
   password, using your keys, `ssh-agent`, `~/.ssh/config` and known
   hosts. Accept the host key outside Foldover, for example by running
   `ssh user@host` once. Password logins cannot work: Foldover runs ssh
   in batch mode, so it never asks
   ([ADR 0028](../adr/0028-sftp-endpoints.md),
   `crates/foldover-net/src/sftp.rs`).
2. Add the folder to the pair as a locator:

   An absolute path, then one under the login's home folder:

   ```text
   sftp://alice@nas.local:2222/srv/photos
   sftp://backup/~/photos
   ```

   The syntax is `sftp://[user@]host[:port]/path`; the path is required.
   Omit the user or port to use your `~/.ssh/config` defaults. `/~` and
   `/~/...` name the login user's home folder
   ([ADR 0028](../adr/0028-sftp-endpoints.md)).

Foldover runs `/usr/bin/ssh -o BatchMode=yes -s -- host sftp` and speaks
SFTP version 3 on that connection. OpenSSH handles keys, the agent, your
`~/.ssh/config`, host keys and jump hosts; a login that still needs a
password fails with ssh's own message
([ADR 0028](../adr/0028-sftp-endpoints.md),
`crates/foldover-net/src/sftp.rs`).

### Trust the server

SFTP cannot pin the folder the way local roots do. Between Foldover's
checks, a process on the server can swap a directory for a symlink and
redirect reads and writes outside the selected folder. Use SFTP folders
only with servers you trust. Foldover needs no server-side helper and
works with an ordinary account; a jailed (chrooted) account still limits
what such a swap can reach ([ADR 0028](../adr/0028-sftp-endpoints.md),
[PLAN.md](../../PLAN.md) section 10 item 7). The app and the CLI show
this warning when you configure an SFTP folder.

### What works the same, and what differs

Previews, filters, `.foldoverignore`, size and age limits, the identity
file, staged writes with content checks, archive browsing, Put Back and
Keep Both all work the same way as on a local folder
([ADR 0028](../adr/0028-sftp-endpoints.md)).

On SFTP, extended attributes and ACLs are not synced, only mode bits and
file modification times in whole seconds are kept, and a local file
with a resource fork fails to copy. Change notifications do not exist,
so "When files change" looks at the folder's top level and recently
active folders every 5 minutes and compares everything hourly
([ADR 0022](../adr/0022-scheduled-runs.md),
[ADR 0028](../adr/0028-sftp-endpoints.md)).

A large copy that breaks off resumes where it stopped when it lands in
a local folder or on a paired Mac; a copy to an SFTP server starts
over. Copies from an SFTP server read each file once instead of twice
([ADR 0034](../adr/0034-resumed-transfers.md)).

While you edit a pair, Foldover warns that an SFTP side cannot keep the
extended attributes you chose to sync, and, between a Mac and an SFTP
server, that the two sides compare names differently; warnings never
block saving ([ADR 0037](../adr/0037-readiness-warnings.md)).

### Durability

SFTP folders only offer Standard durability, which follows the server's
acknowledged-write policy. Full is rejected when you save the pair:
`fsync@openssh.com` applies to files only, so it cannot flush namespace
changes or guarantee a drive-cache flush
([ADR 0028](../adr/0028-sftp-endpoints.md)).

### When the server is unreachable

- Scheduled runs wait, exactly like an unmounted disk: the pair shows
  `waiting` and retries at its schedule
  ([ADR 0028](../adr/0028-sftp-endpoints.md),
  [ADR 0022](../adr/0022-scheduled-runs.md)).
- Manual runs fail with a clear error from ssh
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).
- Saving a pair connects to its own servers, but other pairs' SFTP roots
  are compared by their locators without connecting, so an offline
  server does not block the catalog. One folder written two ways, say
  with another user name or through `~`, is not caught as an overlap
  ([ADR 0028](../adr/0028-sftp-endpoints.md)).

## Two Macs, one folder

- Several Macs can sync the same share or server folder; one syncs at a
  time. A run that finds another Mac's sync running waits with "Waiting
  for <Mac> to finish syncing these folders…", and takes over from one
  that stopped responding two minutes ago. See
  [limitations](limitations.md) for the details
  ([ADR 0032](../adr/0032-root-leases.md)).
