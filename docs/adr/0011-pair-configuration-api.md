# ADR 0011: Authenticated local pair configuration

Status: Implemented API, CLI and native pair controls

## Boundary

`foldoverd` owns one `Service`, which holds one engine and persistent writer.
Configuration commands run serially on a blocking worker, outside Tokio's
reactor. Validation, catalog checks and persistence share that command. Its
permit survives a client disconnect until the command finishes. Health requests
do not acquire the configuration queue or access pair state.

IPC protocol version 2 requires successful `session.hello` negotiation before
private dispatch. A failed renegotiation closes private access on that connection.
Installed-code authentication precedes every private request; typed clients
also authenticate the agent before their handshake and recheck before sending
private parameters. Linux retains the user-ID boundary. Standalone macOS
development clients remain health-only. See [ADR 0006](0006-ad-hoc-client-identity.md).

## Methods and representation

- `pair.create`: validated local roots, name and explicit mode/source.
- `pair.get`: an existing pair ID.
- `pair.update`: ID, expected positive revision and replacement configuration.
- `pair.list`: optional exclusive `after` ID; at most 16 records and a next ID.

IDs are 32 lowercase hex digits, excluding all zero. Roots use byte arrays,
not lossy strings. Names allow at most 1024 UTF-8 bytes; roots at most 4096
bytes. Maximum-length list records fit below the 1 MiB framing limit. Unknown
private fields and invalid domain values are rejected before engine access.

Lists use stable identity order, not mutable names or offsets. They are bounded
queries, not multi-request snapshots; refresh to observe concurrent insertions
behind the cursor. Clients reject malformed, unordered or nonadvancing pages.
Missing pairs, stale revisions, unavailable roots and overlap have distinct
errors. No implicit retry or pair deletion is added.

Batch replies share the same frame budget. Dispatch stops with an explicit
error when the aggregate would exceed it. Earlier commands may have committed:
batches are not transactions, so refresh state before retrying.

## Root selection

Before creation or update, the engine opens both selected roots and compares
held identities and ancestry. It rejects equal/nested/aliased roots, overlap
with any other configured pair, and overlap with its state directory. Updating
a pair excludes only that pair from catalog comparison. The current revision
is checked before filesystem probes, then checked again by the storage driver.

Catalog checks page through existing pairs and fail closed if any existing
root cannot be verified. This deliberately requires reconnecting offline roots
before catalog changes. It prevents guessing about physical aliases until
persistent volume and endpoint identities are implemented.

These are selection-time checks. They do not establish mount continuity,
permissions, cloud-provider policy, free space, capabilities, canaries or
execution freshness. Concurrent namespace changes still require execution-time
revalidation. Configuration never reads file contents or changes endpoints.

## Native controls

The main window lists configured pairs and edits them in a folder-selection
sheet. The mode sentence explains direction and policy. Saving configures a
pair; it does not scan or execute it. The menu bar reports configuration, not
sync success. Unknown or failed catalog loads are not presented as an empty
catalog, and incomplete pages never replace cached rows.

The Swift store converts generated client records inside one actor and returns
native Sendable values to the UI actor. Folder bytes remain separate from display
text, including when an existing path is not UTF-8. Editing preserves the saved
revision and retains unsaved fields on error. Cancel and Save have keyboard
actions; save-in-progress controls prevent duplicate submission. Closing a
successful editor selects its saved pair.

## Verification and remaining work

Tests cover serialized competing creations, rejected mutations, revisions,
raw paths, all modes, identity paging, size bounds, root/catalog/state overlap,
offline refusal, failed negotiation and malformed replies. Process smoke
exercises CLI create/read/update, duplicate and stale rejection, and persistence
through an agent crash. macOS CI requires private denial for standalone clients
and successful configuration through the signed embedded release helpers.

If a request times out, its accepted command may still commit. Refresh before
retrying. A catalog overlap prevents duplicate creation over the same roots.
Native store tests cover paged publication, stale refreshes, failed saves,
direction, raw paths and post-save selection. Native build/tests require macOS
CI; interactive first-pair timing, VoiceOver and permission/update flows remain
unverified. Endpoint identities, readiness, snapshots, plans and execution are
still required.
