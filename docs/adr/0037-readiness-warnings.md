# ADR 0037: Readiness warnings

Status: Implemented

## Context

PLAN.md §3.6: before a person saves a pair, the app shows what will not
work as they might expect. Overlapping roots are already blocked at
creation; these checks warn instead, so they must never block saving,
must tolerate folders that come and go, and must work while the person
is still editing, before any pair exists.

## Decision

`pair.check` (local protocol 13) takes the same input as `pair.create`
and returns `{ "warnings": [...] }`, each warning naming a side and a
kind, and a provider for `cloudManaged`. `Engine::check_pair` opens both
roots like creation does; a root that cannot be opened yields no
warnings for its side, because saving reports that itself. Three kinds:

- **cloudManaged:** a local root inside a folder another sync engine
  manages, detected by path against the agent user's home:
  `~/Library/Mobile Documents` is iCloud Drive;
  `~/Library/CloudStorage/<Name>-<account>` names the provider before
  the first `-`; `~/Desktop` and `~/Documents` count while iCloud's own
  Desktop & Documents folders exist. The home lookup and the path and
  existence checks live in the `foldover-os` driver as a pure function
  over a given home plus a thin wrapper reading the real one.
- **metadataUnsupported:** the pair syncs extended attributes a side
  cannot keep: an SFTP side always, a local side whose filesystem
  declares extended attributes unsupported ([ADR
  0017](0017-filesystem-feature-declarations.md)). Silence counts as
  support: only an explicit declaration warns.
- **namesDiffer:** one side compares names case-insensitively and the
  other exactly (`Endpoint::name_matching()`, [ADR
  0030](0030-name-clashes.md)); the warning names the side that ignores
  case. Such names become conflicts to rename.

The Rust engine returns kinds and data, never sentences: the app's pair
editor builds the English from the warning kind and the Folder A/B names
it already uses, and refreshes whenever a side or a metadata choice
changes. The CLI prints the wire form as `foldover pairs check` with the
same arguments as `add`; the FFI exports `check_pair` mirroring
`pair_create`.

## Consequences

- A warning is advisory only; overlap remains the one check that blocks.
- Checking an SFTP or shared folder opens it, so warnings appear once
  the server or Mac answers; while it cannot be reached, that side
  simply has no warnings.
- A volume that does not report its name matching counts as ignoring
  case (ADR 0030's rule), so Mac-to-SFTP pairs warn that case-only
  renames become conflicts even when the Mac's volume happens to be
  case-sensitive.
