//! Undo the newest writing run (ADR 0029, PLAN.md §3.8). What it created
//! goes to a new archive run; what it replaced or archived comes back from
//! its archive. Each entry is undone only while its path still holds what
//! the run left there; anything else is listed, never forced. Undone paths
//! get their base from before the run back, so the next preview shows what
//! syncing again would redo.
//!
//! ```text
//! journal (write order)          undo (reverse order)
//! 0 create  photos/              4 put back  old/notes.txt
//! 1 create  photos/a.jpg         3 put back  report.txt, after archiving
//! 2 meta    readme.md              what replaced it
//! 3 replace report.txt           2 listed: earlier metadata is not kept
//! 4 archive old/notes.txt        1 archive   photos/a.jpg
//!                                0 remove    photos/ (empty again)
//! ```

use foldover_endpoint::ApplyError;
use foldover_state::Journal;
use foldover_types::EntryKind;

use super::*;

/// What undoing a run did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct UndoReport {
    completion: Completion,
    archive: String,
    removed: u64,
    restored: u64,
    beside: u64,
    not_undone: Vec<Failure>,
}

impl UndoReport {
    pub fn completion(&self) -> Completion {
        self.completion
    }

    /// The undo's own folder under `.foldover/archive`, holding what the
    /// undone run had created.
    pub fn archive(&self) -> &str {
        &self.archive
    }

    /// Entries the run created, now archived.
    pub fn removed(&self) -> u64 {
        self.removed
    }

    /// Entries the run replaced or archived, back at their paths.
    pub fn restored(&self) -> u64 {
        self.restored
    }

    /// Archived entries put back beside a newer entry at their path.
    pub fn beside(&self) -> u64 {
        self.beside
    }

    /// Entries left as they are, and why.
    pub fn not_undone(&self) -> &[Failure] {
        &self.not_undone
    }
}

const CHANGED: &str = "Changed since the sync; left as it is";
const METADATA_ONLY: &str = "Only its metadata changed, and the earlier metadata is not kept";
const NOT_ARCHIVED: &str = "The earlier version is no longer in the archive";
const EMPTY_FOLDER: &str = "Empty folders are not recreated";
const OCCUPIED: &str = "Something new holds the path it moved from; left as it is";
const UNREADABLE: &str = "This path or a related folder could not be read; left unchanged";

impl PairRun {
    /// Undo `journal`, the pair's newest writing run. The run's roots must
    /// still be the recorded ones.
    pub fn undo(
        self,
        journal: Journal,
        started: SystemTime,
        commit: &mut impl FnMut(u64, Commit<'_>) -> Result<(), crate::Error>,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<UndoReport, RunError> {
        let options = &self.options;
        for side in [Side::Left, Side::Right] {
            if options.durability() == Durability::Full
                && matches!(self.endpoints.endpoint(side), Endpoint::Sftp(_))
            {
                return Err(RunError::UnsupportedDurability { side });
            }
        }
        // Put back needs the archive a folder without records lacks.
        if let Some(side) = self.pair.draft().unrecorded() {
            return Err(RunError::UndoUnavailable { side });
        }
        // Only a recorded first run can have written anything.
        let (Some(left), Some(right)) = (
            self.identities.side(Side::Left),
            self.identities.side(Side::Right),
        ) else {
            return Err(RunError::NothingToUndo);
        };
        let identities = [left, right];
        self.check_roots(identities)?;

        let revision = self.pair.revision();
        let scope = RunScope::new(started, options.clone());
        let name = scope.name().to_owned();
        // An undo writes too: only while it holds both folders' leases.
        let leases = self.take_leases(identities, &name, control, &NoProgress)?;
        let mut control = || leases.check(control);
        let control = &mut control;
        let mut report = UndoReport {
            archive: name.clone(),
            ..UndoReport::default()
        };
        let mut put_back: Vec<(Side, SyncPath)> = Vec::new();
        let mut folders: Vec<&JournalEntry> = Vec::new();
        let entries: Vec<&JournalEntry> = journal.entries.iter().rev().collect();

        // Each batch commits its base restores — and the hold that keeps
        // scheduled runs from redoing the undo — before changing a file.
        // A crash in between leaves a path holding what the run left with
        // its pre-run base: the next run finds both sides equal again, or
        // a conflict, and never propagates a half-done undo.
        for batch in entries.chunks(COMMIT_BATCH) {
            if control() == ScanControl::Cancel {
                report.completion = Completion::Cancelled;
                break;
            }
            self.check_roots(identities)?;
            let mut steps = Vec::new();
            for entry in batch {
                let step = match self.plan_undo(entry) {
                    Ok(step) => step,
                    Err(message) => {
                        report.not_undone.push(Failure {
                            path: Some(entry.path.clone()),
                            message: message.into(),
                        });
                        continue;
                    }
                };
                // A folder the run removed comes back with the first entry
                // put back inside it; decide once every child is handled.
                if matches!(entry.action, JournalAction::Archive(version) if version.kind() == EntryKind::Directory)
                {
                    folders.push(entry);
                    continue;
                }
                steps.push((*entry, step));
            }
            if steps.is_empty() {
                continue;
            }
            let restores: Vec<BaseUpdate> = steps
                .iter()
                .filter(|(_, step)| *step != Step::PutBackBeside)
                .flat_map(|(entry, _)| restore(entry))
                .collect();
            commit(revision, Commit::Undoing { updates: &restores }).map_err(RunError::Commit)?;
            let mut undid = Vec::new();
            for (entry, step) in steps {
                match self.undo_entry(entry, step, &journal.run, &scope, control) {
                    Ok(Placed::AtPath) if step == Step::Remove => report.removed += 1,
                    Ok(Placed::AtPath) => {
                        report.restored += 1;
                        let restored = entry.moved_from.as_ref().unwrap_or(&entry.path);
                        put_back.push((entry.side, restored.clone()));
                    }
                    Ok(Placed::Beside) => report.beside += 1,
                    Err(message) => {
                        report.not_undone.push(Failure {
                            path: Some(entry.path.clone()),
                            message,
                        });
                        continue;
                    }
                }
                undid.push(entry.seq);
            }
            // Undoing again after a cancel or crash skips these; without
            // this, a retry finds them back and lists each as changed or
            // no longer archived.
            let undid = Commit::Undid {
                run: &journal.run,
                seqs: &undid,
            };
            commit(revision, undid).map_err(RunError::Commit)?;
        }

        if let Some(error) = leases.failure() {
            return Err(error.into());
        }

        let mut refilled = Vec::new();
        if report.completion == Completion::Finished {
            for folder in folders {
                // An earlier, cancelled attempt may have refilled it.
                let exists = self
                    .observation(folder.side)
                    .entries()
                    .get(&folder.path)
                    .is_some_and(|current| current.version().kind() == EntryKind::Directory);
                if exists
                    || put_back.iter().any(|(side, path)| {
                        *side == folder.side && folder.path.is_ancestor_of(path)
                    })
                {
                    report.restored += 1;
                    refilled.extend(restore(folder));
                } else {
                    report.not_undone.push(Failure {
                        path: Some(folder.path.clone()),
                        message: EMPTY_FOLDER.into(),
                    });
                }
            }
        }

        // A finished undo spends the journal; a cancelled one keeps it, so
        // undoing again picks up what is left.
        if report.completion == Completion::Finished {
            let last = Commit::Undone {
                run: &journal.run,
                updates: &refilled,
            };
            commit(revision, last).map_err(RunError::Commit)?;
        }
        Ok(report)
    }

    /// What undoing `entry` needs, judged from the fresh observation, or
    /// why it stays as it is.
    fn plan_undo(&self, entry: &JournalEntry) -> Result<Step, &'static str> {
        // Missing rows from a failed scan cannot authorize a restore or rename.
        let names = self
            .endpoints
            .endpoint(Side::Left)
            .name_matching()
            .either(self.endpoints.endpoint(Side::Right).name_matching());
        for path in std::iter::once(&entry.path).chain(entry.moved_from.as_ref()) {
            let canonical = SyncPath::try_from(crate::plan::name_key(path.as_bytes(), names))
                .expect("a name key is a valid path");
            for observation in [&self.left, &self.right] {
                if observation.entries().contains_key(path) {
                    continue;
                }
                // Archived and moved-away journal names may no longer be in
                // either observation or the base used to build `uncertain`.
                // A changed listing still cannot prove these names absent.
                if observation.unstable_directories().iter().any(|directory| {
                    directory.as_ref().is_none_or(|directory| {
                        let directory =
                            SyncPath::try_from(crate::plan::name_key(directory.as_bytes(), names))
                                .expect("a name key is a valid path");
                        directory == canonical || directory.is_ancestor_of(&canonical)
                    })
                }) {
                    return Err(UNREADABLE);
                }
            }
            for unread in self
                .left
                .unavailable()
                .iter()
                .chain(self.right.unavailable())
                .chain(&self.uncertain)
            {
                let unread = SyncPath::try_from(crate::plan::name_key(unread.as_bytes(), names))
                    .expect("a name key is a valid path");
                if canonical == unread
                    || canonical.is_ancestor_of(&unread)
                    || unread.is_ancestor_of(&canonical)
                {
                    return Err(UNREADABLE);
                }
            }
        }
        let current = self.observation(entry.side).entries().get(&entry.path);
        let left_by_run = |version: EntryVersion| current.is_some_and(|c| c.version() == version);
        match entry.action {
            JournalAction::Metadata(_) => Err(METADATA_ONLY),
            JournalAction::Create(version) if left_by_run(version) => Ok(Step::Remove),
            JournalAction::Replace(version) if left_by_run(version) => Ok(Step::Replace),
            JournalAction::Create(_) | JournalAction::Replace(_) => Err(CHANGED),
            JournalAction::Archive(_) if current.is_none() => Ok(Step::PutBack),
            JournalAction::Archive(_) => Ok(Step::PutBackBeside),
            // A move goes back only where the old path is still free.
            JournalAction::Move(version) if left_by_run(version) => {
                let from = entry.moved_from.as_ref().ok_or(CHANGED)?;
                match self.observation(entry.side).entries().contains_key(from) {
                    false => Ok(Step::MoveBack),
                    true => Err(OCCUPIED),
                }
            }
            JournalAction::Move(_) => Err(CHANGED),
        }
    }

    fn undo_entry(
        &self,
        entry: &JournalEntry,
        step: Step,
        run: &str,
        scope: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Placed, String> {
        let endpoint = self.endpoints.endpoint(entry.side);
        let current = self.observation(entry.side).entries().get(&entry.path);
        let durability = self.pair.draft().options().durability();
        let message = |error: ApplyError| match error {
            ApplyError::NotArchived(_) => NOT_ARCHIVED.to_owned(),
            error => error.to_string(),
        };
        let put_back = |run: &str| {
            endpoint
                .put_back(run, &entry.path, durability)
                .map(|placed| {
                    if placed == entry.path {
                        Placed::AtPath
                    } else {
                        Placed::Beside
                    }
                })
                .map_err(message)
        };
        match (step, current) {
            (Step::Remove, Some(current)) => endpoint
                .remove(&entry.path, current, scope, control)
                .map(|()| Placed::AtPath)
                .map_err(message),
            (Step::Replace, Some(current)) => {
                endpoint
                    .remove(&entry.path, current, scope, control)
                    .map_err(message)?;
                put_back(run).inspect_err(|_| {
                    // Nothing to bring back: return what the run left.
                    let _ = endpoint.put_back(scope.name(), &entry.path, durability);
                })
            }
            (Step::PutBack | Step::PutBackBeside, _) => put_back(run),
            (Step::MoveBack, Some(current)) => {
                let from = entry.moved_from.as_ref().ok_or(CHANGED)?;
                endpoint
                    .move_entry(&entry.path, current, from, scope, control)
                    .map(|()| Placed::AtPath)
                    .map_err(message)
            }
            (Step::Remove | Step::Replace | Step::MoveBack, None) => Err(CHANGED.to_owned()),
        }
    }
}

/// How one journal entry is undone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Archive what the run created.
    Remove,
    /// Archive what the run left and put back what it replaced.
    Replace,
    /// Put back what the run archived, at its path.
    PutBack,
    /// Something holds the path now: put back beside it; the base stays.
    PutBackBeside,
    /// Rename what the run moved back to where it was.
    MoveBack,
}

/// Where an undone entry ended up.
enum Placed {
    AtPath,
    /// Beside a newer entry, as `name (restored).ext`.
    Beside,
}

/// The base an undone path had before the run; a move's new path had
/// none, and its old one gets back what it had.
fn restore(entry: &JournalEntry) -> Vec<BaseUpdate> {
    let path = entry.moved_from.as_ref().unwrap_or(&entry.path);
    let mut updates = Vec::new();
    if entry.moved_from.is_some() {
        updates.push(BaseUpdate::Forget(entry.path.clone()));
    }
    updates.push(match before(entry) {
        Some(base) => BaseUpdate::Record(path.clone(), base),
        None => BaseUpdate::Forget(path.clone()),
    });
    updates
}

/// What the undone path's base was before the run, as far as the folder
/// still holds it. A move goes back as the run left it: metadata it
/// brought stays, as undo leaves metadata-only changes, so its side
/// records that version. Same content on both sides keeps the pair.
fn before(entry: &JournalEntry) -> Option<BaselineEntry> {
    let base = entry.before?;
    let JournalAction::Move(moved) = entry.action else {
        return Some(base);
    };
    let (left, right) = match entry.side {
        Side::Left => (moved, base.right()),
        Side::Right => (base.left(), moved),
    };
    Some(BaselineEntry::new(left, right).unwrap_or(base))
}
