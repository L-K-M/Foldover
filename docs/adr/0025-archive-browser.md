# ADR 0025: Archive browser and Put Back

Status: Implemented

## Decision

The app's **Browse Archive** sheet and `foldover archive` list each side's
archived runs (newest first) and the files and links a run archived, and put
an item back, or everything a run archived. Requests go through the agent
(`archive.runs`, `archive.items`, `archive.putBack`, `archive.putBackAll`);
clients never touch `.foldover`.

```text
.foldover/archive/2026-10-18T09-30-00.000Z/docs/report.txt
                  └ run (start time, UTC) ┘└ original path ┘
```

**Put back never replaces.** The item moves with a no-replace rename to its
original path, creating missing parent folders. When that path is taken,
for example by the newer version that displaced it, the item goes beside it
as `docs/report (restored).txt`, then `(restored 2)` and so on. Long names
lose the end of their stem, never the extension, and stay valid UTF-8.

- The restored item is an ordinary new entry: the next sync copies it to
  the other folder. Put back changes no sync state.
- Only canonical run names are accepted, so a request cannot address
  anything outside the archive; paths naming `.foldover` or temporaries are
  refused.
- A put-back is refused while the pair previews or syncs, so a running
  job's plan stays accurate. A racing run cannot lose data either way:
  both sides only create with no-replace.
- **Put Back All** lists the run first, then puts back each item the same
  way; one item's failure does not stop the rest, and the result counts
  items restored, items placed beside newer versions, and failures.
- Items are paged in path order (100 per response, `after` cursor); runs
  are capped at 1,000 per response.

## Consequences

- Each page walks the whole run folder. Runs that archived hundreds of
  thousands of items list slowly.
- Folders are not archive items: removals only delete empty folders, so
  putting a file back recreates the folders above it.
- Leftover temporaries archived by recovery are listed but cannot be put
  back under their temporary name; copy them out by hand.
