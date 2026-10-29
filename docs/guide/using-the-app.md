# Using the app

Foldover's window is built like Mail or Xcode: pairs in the sidebar, the
selected pair in the middle, and details in an inspector on the right.
Every toolbar button and every command in the context menus is also in
the menu bar, and the menus own the keyboard shortcuts. The window's
commands act on the selected pair and turn off while the pair editor or a
dialog is open.

Click an **ⓘ** button beside a setting for instructions and tradeoffs.
Press Escape or click outside to close the popover. Warnings and the live
sync plan stay visible without opening help.

## The main window

- **Sidebar.** Every pair with its direction, name and status. A spinner
  shows a pair at work, a warning triangle one that needs you, and a badge
  its open conflicts. Control-click a pair for its commands. To remove the
  selected pair, press Delete or choose Pair › Remove Pair…; Foldover asks
  first, and your folders stay in place. Drop one or two folders on the
  sidebar to open the New Pair editor with them filled in. The line at the
  bottom shows the background agent's state, with the one button that
  fixes it when something is wrong: Turn On, Open Login Items or Resume
  Syncing.
- **Detail.** The pair's two folders and the sentence that says what a
  sync does, then its status: progress while it works, what the last sync
  did, and Undo Sync or Review Conflicts when they apply. Below, the table
  of planned changes. Once a preview lists changes, choose All, Copies,
  Updates, Archives, Conflicts or Moves from the Show pop-up above the
  table (View › Show has the same choices), or type in the toolbar's
  search field to show only changes whose path contains your text.
- **Inspector.** The selected change, with every choice you can make for
  it; the pair's mode, folders, schedule and a summary of its skip rules
  and limits, with Edit Pair… and Browse Archive…; and its ten most recent
  syncs. Show or hide it with View › Show Inspector or the toolbar button.
- **Toolbar.** Preview, Sync Now (Stop while a job runs), New Pair, the
  inspector and search. View › Customize Toolbar… adds Sync All, Archive
  and Edit Pair.

The window remembers the selected pair and whether the inspector is
shown. Closing it leaves Foldover running in the menu bar; open it again
from the menu bar extra, the Dock or the Window menu.

## Editing a pair

You can save settings while a preview or sync runs. That job keeps the
settings it started with; the next preview or sync uses your saved changes.

When you create a Backup whose source is your home folder, the Skip section
starts with cache exclusions. You can turn them off or edit the rules.
Add Exclusions offers Trash, logs, Xcode DerivedData and developer download
caches as separate choices. These rules leave existing target copies alone.
Existing pairs keep their current rules until you change them.

## Reviewing changes

Select a change and press Space to look at its file with Quick Look, or
double-click it; Quick Look and Show in Finder need the file to be in a
folder on this Mac. Control-click the change, or use the Pair menu or the
inspector, to keep Folder A's or Folder B's version of a conflict, keep
both versions of a file both folders changed, do a copy, update or archive
the other way (for example, restore an archived item instead), skip it
once, or always skip it. Edit › Copy copies the selected change's path.

## Undo

After Sync Now from the window copies or archives something, Edit › Undo
shows **Undo Sync of “name”** until you preview or sync that pair again.
It does what the Undo Sync button does: files the sync replaced or
archived come back, and what it created goes to the archive. Sync All and
syncs started from the menu bar extra are not listed there; the Undo Sync
button covers them. See [Archives and Undo](limitations.md#archives-and-undo) for what Undo
cannot reverse. Text you type, in the pair editor or the search field,
undoes as usual.

## The archive

Pair › Browse Archive… opens the pair's archive in its own window, so it
can stay open while you work. Each pair gets one window. Choose Folder A
or Folder B in the toolbar and a sync on the left; select items, even
several, and click Put Back. Put Back All… puts back everything that sync
archived in that folder, after asking. Put Back never replaces anything:
if something is in the way, the item goes beside it with “(restored)” in
its name.

## The menu bar

The menu bar extra lists every pair with its status; scroll the list to
reach more pairs. Click a pair to show it in the window, or click its
button to sync it. While it works, that button stops its job instead.
Commands stay visible below the list. The extra also has Sync All,
Pause Automatic Syncs (for an hour, four hours or until you resume),
Open Foldover, Settings… and Quit Foldover. The Pair menu offers the same
Sync All and Pause Automatic Syncs.

## Keyboard shortcuts

| Command | Menu | Shortcut |
|---|---|---|
| New Pair… | File | ⌘N |
| Find… (the search field) | Edit | ⌘F |
| Undo Sync of “name” | Edit | ⌘Z |
| Copy the selected change's path | Edit | ⌘C |
| Remove the selected pair | Edit › Delete | Delete (in the sidebar) |
| Show: All, Copies, Updates, Archives, Conflicts, Moves | View | ⌘1 to ⌘6 |
| Show or hide the sidebar | View | ⌃⌘S |
| Show or hide the inspector | View | ⌃⌘I |
| Preview Changes | Pair | ⌘R |
| Sync Now | Pair | ⌘S |
| Stop | Pair | ⌘. |
| Sync All | Pair | ⌥⌘S |
| Browse Archive… | Pair | ⇧⌘A |
| Edit Pair… | Pair | ⌘I |
| Quick Look the selected change | | Space |
| Settings… | Foldover | ⌘, |

In the pair editor, Return saves when Save Pair is enabled (except while
typing skip rules), and Escape cancels.
