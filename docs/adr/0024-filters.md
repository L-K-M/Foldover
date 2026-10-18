# ADR 0024: Filters

Status: Implemented (name and path rules, size limit); date terms pending

## Decision

Each pair has gitignore-style exclusion rules (state schema v8), edited in
the app or with `--exclude`. A `.foldoverignore` file in either root adds
rules. Matching uses the `ignore` crate's gitignore semantics: `*.tmp`,
`build/` (folders only), `/top` (anchored), and `!keep.tmp` to re-include.

**One filter for both sides.** Pair rules and both roots' ignore files are
combined and applied to both folders, so an excluded entry is absent on
both sides. Against the base that reads as "both absent", which forgets
the entry instead of deleting it; removing a rule makes entries appear on
both sides like new ones. PLAN.md §3.7's re-baseline after filter edits is
therefore unnecessary, and the base is kept.

- Excluded folders are not entered. Built-in exclusions (`.foldover`,
  temporaries, `.DS_Store`) are not rules and cannot be switched off.
- Rules are checked when a pair is saved; invalid rules are rejected.
  A `.foldoverignore` that is not UTF-8 text stops the run before planning.
- Excluding an AppleDouble pattern such as `._*` skips sidecars instead of
  stopping the scan.
- **Size limit** (state schema v10): files larger than the limit are not
  read. Sizes differ between folders, so a path skipped in either folder
  is removed from both observations before planning; with no base match
  it is forgotten, never deleted. Removing the limit can surface the two
  versions as a conflict.
- **Always Skip** on a preview row adds a rule matching exactly that path:
  anchored with `/`, with `\ * ? [ ] { }` and trailing spaces escaped
  (`docs/a*.txt` → `/docs/a\*.txt`). Names that are not UTF-8 cannot be
  written as rules and are not offered.

## Consequences

- A folder deleted on one side cannot be removed on the other while it
  still holds excluded entries; that removal fails and is reported.
- Modification-date terms are not implemented.
