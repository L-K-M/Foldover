# Using the app

Foldover's window is built like Mail or Xcode: pairs in the sidebar, the
selected pair in the middle, and details in an inspector on the right.
Everything a button does is also in the menu bar, with the same keyboard
shortcut wherever you are in the window.

## The main window

- **Sidebar.** Every pair with its direction, name and status. A spinner
  shows a pair at work, a warning triangle one that needs you, and a badge
  its open conflicts. Control-click a pair for its commands. Press Delete to
  remove the selected pair; Foldover asks first, and your folders stay in
  place. Drop one or two folders on the sidebar to start a new pair with
  them. The line at the bottom shows the background agent, with the one
  button that fixes it when it is off.
- **Detail.** The pair's two folders and the sentence that says what a
  sync does, then its status: progress while it works, what the last sync
  did, and Undo Sync or Review Conflicts when they apply. Below, the table
  of planned changes. Choose a kind of change above the table, or type in
  the toolbar's search field to filter by name.
- **Inspector.** The selected change, with every choice you can make for
  it; the pair's mode, folders, schedule and skip rules; and its recent
  syncs. Show or hide it with View › Show Inspector or the toolbar button.
- **Toolbar.** Preview, Sync Now (Stop while a job runs), New Pair, the
  inspector and search. View › Customize Toolbar… adds Sync All, Archive
  and Edit Pair.

The window remembers the selected pair and whether the inspector is
shown. Closing it leaves Foldover running in the menu bar; open it again
from the menu bar extra, the Dock or the Window menu.

## Reviewing changes

Select a change and press Space to look at its file with Quick Look, or
double-click it. Control-click it, use the Pair menu or the inspector to
keep one folder's version of a conflict, keep both, copy a change the other
way, skip it once, or always skip it. Edit › Copy copies the selected
change's path.

## Undo

After you sync a pair from the window, Edit › Undo shows **Undo Sync of
“name”**. It does what the Undo Sync button does: files the sync replaced
or archived come back, and what it created goes to the archive. Text you
type, in the pair editor or the search field, undoes as usual.

## The archive

Pair › Browse Archive… opens the pair's archive in its own window, so it
can stay open while you work. Each pair gets one window. Choose Folder A or
Folder B in the toolbar and a sync on the left; select items, even
several, and click Put Back.

## Keyboard shortcuts

| Command | Menu | Shortcut |
|---|---|---|
| New Pair… | File | ⌘N |
| Find (focus the search field) | Edit | ⌘F |
| Undo Sync of “name” | Edit | ⌘Z |
| Copy the selected change's path | Edit | ⌘C |
| Remove the selected pair | Edit › Delete | Delete (in the sidebar) |
| All, Copies, Updates, Archives, Conflicts, Moves | View | ⌘1 to ⌘6 |
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

In the pair editor, Return saves (except while typing skip rules) and
Escape cancels.
