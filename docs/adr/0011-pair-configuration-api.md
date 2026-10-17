# ADR 0011: Authenticated local pair configuration

Status: Implemented API and CLI; native pair UI pending

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

Ad-hoc Release builds disable Xcode's base-entitlement injection. Otherwise
Xcode adds `get-task-allow` and the installed-code policy correctly refuses the
bundle. Process smoke inspects signed app/helper entitlements before private calls.

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

## Verification and remaining work

Tests cover serialized competing creations, rejected mutations, revisions,
raw paths, all modes, identity paging, size bounds, root/catalog/state overlap,
offline refusal, failed negotiation and malformed replies. Process smoke
exercises CLI create/read/update, duplicate and stale rejection, and persistence
through an agent crash. macOS CI requires private denial for standalone clients
and successful configuration through the signed embedded release helpers.

If a request times out, its accepted command may still commit. Refresh before
retrying. A catalog overlap prevents duplicate creation over the same roots.
Native pair controls, endpoint identities, readiness, snapshots, plans and
execution remain separate required work.
