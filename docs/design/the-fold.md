# The Fold: a design note

Foldover's central object is a pair: two folders that should end up
alike. Its central transformation is the plan: a preview that says what
a sync would copy, archive, rename or leave for a person, and the run
that carries it out. PLAN.md calls the preview the product. The moment
worth making delicious is the one between reading the plan and
committing to it, and the relief after.

## Concept 1: The Fold

**Inspired by** the name, and by the pair itself: two halves of one
sheet, with a mode that decides which half carries wet ink. Sync moves
ink both ways; Mirror and Backup only from the source.

**Before.** The detail pane is the icon's navy field. On it lies a
sheet of ivory origami paper creased down the middle, laid out like a
book's spread: Folder A's page on the left, B's on the right, and an
empty gutter between them. Every planned change is one 26 pt line on
both pages. Both pages read left to right. A folder's own version is in
its ink (A navy, B cerulean). Where a copy will land, its name is faint.
An archive's other half says "no copy". The page that changes carries a
mark in its outer margin: a disc in the arriving ink pointing the way it
travels, an archive box, a tag for metadata, or a pin on both pages for
a conflict. The verb and size end B's page. ← or → on a line stages a
turn and names it ("Archive in Folder A"). Return runs it.

**During.** Drag the dog-ear toward the crease. Nothing moves for 8 pt,
then the half swings up in perspective. Its words give way to type
bars as it starts to turn, so perspective never squeezes letters into
slivers. The faint names underneath fill with its ink as it approaches,
fully by the time it stands upright, so the outcome can be seen before
committing. Upright is the threshold: the trackpad ticks there. Let go
before upright and the leaf falls back. Let go past it and the leaf
falls, presses, and the sync starts. Like origami paper, the sheet is
colored on its back: past upright the leaf turns cerulean, and once it
lands the run's progress shows in white on a flat plate of that blue.

**After.** The leaf peels open, then a cerulean card stamps onto the
page it pressed: the run's own counts in large white figures. Undo
lifts it.

**You have to see this:** the names filling with ink under the
approaching leaf, the blue back turning over, then the thump. Folded,
the sheet is the icon: ivory and cerulean on navy.

**Everyday:** direction is spatial, ⌘S folds automatically, and every
command is in menus and VoiceOver actions.

## Concept 2: The Work Order

**Inspired by** the review token: an approval is bound to the exact plan
that was reviewed, and a pair revision makes old plans obsolete.

**Before.** Previewing feeds a receipt out of a slot at the top of the
pane: one monospaced line per change, totals at the foot, and the plan's
fingerprint printed as a short code. **During.** To sync, drag the
receipt down past its serrated edge. It tears at 60 pt and slides onto
a spike in the inspector. Edit a filter and a red VOID stamp lands on
the stale receipt while a fresh one prints beside it. **After.** The
spike holds every run. Thumbing through it shows each run's actual
counts stamped over the planned ones. Undo pulls the top receipt off
the spike.

**You have to see this:** the VOID stamp slamming down the moment the
plan stops matching the pair.

**Everyday:** the receipt is a dense, legible list. Printing animates
only the first lines, then fast-feeds.

**Limit:** the engine returns a preview whole, so the printing is
presentation, not streaming.

## Concept 3: The Difference Field

**Inspired by** previews with tens of thousands of changes, where the
real question is "where are the dangerous ones?"

**Before.** Above the list, the whole plan is drawn as one strip in path
order. Each change is a hairline mark, colored by kind, its height set
by size. Dense blue bands are new photos; a tight cluster of archive
marks under `2024/drafts` stands out. **During.** Drag across the strip
to select a range: past 6 px the list jumps there. A loupe under the
pointer reads paths in large type, flickering like a slot reel and
settling on folder names. While comparing, the strip grows from left
to right. **After.** The strip stays as a record, labeled "plan before
this run," beside the actual counts.

**You have to see this:** clicking Archives and watching 10,000 marks
drop away to the six that remove files.

**Everyday:** the strip is optional, the list stays the direct route,
and VoiceOver gets a summary by folder.

**Limit:** it needs every page of the plan. A partial map must say
"800 of 10,000 mapped."

## The commitment: The Fold

The Fold is the only concept that comes from what this app is named for
and what it does: it makes two halves match. A ten-second demo needs no
caption: the leaf rises, falls, presses and opens on what happened.

The look comes from the app's icon, not from stationery: colored paper
stock, crisp facets lit from the top left, soft cast shadows, SF Pro
throughout, no ornament. The two inks differ in lightness as well as hue,
so they hold in grayscale.

It also makes the tool better, not just prettier:

- **Direction is position.** Something on the left goes right, and both
  versions of a path sit on the same line of facing pages. A conflict
  shows both inks; a removal shows its absence.
- **The commit is deliberate.** Today's Sync button runs on one click.
  The fold needs real travel past a physical threshold, falls back
  forgivingly, and a plan that needs approval holds the leaf upright
  until the dialog is answered.
- **It holds up at scale.** The sheet stays a list: lazy, fixed-height
  lines, grouped under folder headings, keyboard-complete.

The mode is visible in the motion. A one-way pair has one dog-ear, on
its source. A two-way pair has two, because either half can fold; ⌘S
folds the half with more to carry. A disconnected folder's half is drawn
as a dashed outline: the page is missing.

## Nothing in the gutter

The first build printed each line's direction disc, the folder and mode
glyphs, the selection band and the run's card on the crease or across
it, mirrored the names against it, ran the back's facet ridge through
its words, and swung the leaf's text into slivers. A fold reads as
paper only if it behaves like a printed spread:

- **The gutter holds the crease and nothing else.** Only the crease,
  light and shadows cross it. Each page has its own margins, its own
  selection, its own folder label, and its own reading of every line,
  so a leaf carries one page whole.
- **Marks hang in the outer margins**, on the page that changes: A's at
  its left edge, B's at its right edge, clear of the overlay scroller.
- **Anything that is not a line keeps to one page.** The run's card lands
  on the page the leaf pressed. Messages go on the page the pair writes
  to, or the page that is there when a folder is missing. The mode
  glyph moves above the paper, beside the plan's sentence.
- **Words never sit on a ridge or a turning plane.** The back is flat
  where it carries words. The leaf's words give way to type bars by
  about 25°, and the page under the closing leaf fades rather than show
  cut-off words past its edge. Under Reduce Motion the face fades out
  before the back fades in, so neither lies half-transparent over a
  page.
- **A click on a line only selects it.** Marks still stage turns when
  clicked, but a line's middle is now the empty gutter, not a mark.

## What I deliberately did not do

- **No claims about lines after a run.** A routine sync plans afresh, so
  the preview is a forecast. Matching counts cannot prove which files
  moved. The opened sheet shows the run's own counts, not lines inked
  "done". For the same reason I cut a show-through progress effect that
  revealed lines in order.
- **No flick to commit**, and no single key that archives: turns are
  staged, named, then confirmed.
- **No new shortcuts or engine changes.** ⌘S, the review contract, the
  approval rules and the IPC are untouched.
- **No restyling of supporting surfaces.** The sidebar, inspector,
  editor, archive browser, Settings and menu bar stay native, so the
  sheet gets the attention.
- **No noise from the background.** Paper sounds answer only gestures,
  never a scheduled run, and respect macOS's interface-sound setting and
  a switch in Settings.
- **No automatic re-preview after a run** to prove "folders match":
  that would add a scan to every sync, which is a product decision.
- **No bitmap textures.** The grain is procedural and fixed-seed, and
  the sounds are synthesized by a committed script, so the branch stays
  reproducible.
