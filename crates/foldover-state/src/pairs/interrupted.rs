//! Writing runs the agent admitted and never saw end (ADR 0055): after a
//! restart they show as interrupted, a schedule may continue them, and the
//! run that continues one keeps its journal so Undo covers both.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_db::InterruptedRow;
pub use foldover_db::RunEnd;
use foldover_types::PairId;

use super::{Error, PairStore, database_revision};

const MANUAL: &str = "manual";
const SCHEDULED: &str = "scheduled";
const AUTO: &str = "auto";

/// What started a writing run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritingRun {
    Manual,
    Scheduled,
}

/// Whether an interrupted run continues by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resume {
    /// The schedule continues it at its first chance.
    Auto,
    /// It waits until a person syncs the pair.
    Manual,
}

/// A writing run the agent was still running when it stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptedRun {
    pub pair: PairId,
    pub started: SystemTime,
    pub run: WritingRun,
    /// The pair revision it ran under.
    pub revision: u64,
    pub resume: Resume,
    /// Agent starts that found it, or the run it continued, running
    /// after an unclean stop.
    pub unclean: u32,
}

impl PairStore {
    /// The pair's writing run starts under `revision`. It continues the
    /// journal of an unfinished run before it, unless that journal began
    /// before `chain_cutoff`.
    pub fn admit_run(
        &mut self,
        id: PairId,
        started: SystemTime,
        run: WritingRun,
        revision: u64,
        resume: Resume,
        chain_cutoff: SystemTime,
    ) -> Result<(), Error> {
        let row = InterruptedRow {
            pair: id.as_bytes().to_vec(),
            started_ms: milliseconds(started),
            trigger: match run {
                WritingRun::Manual => MANUAL,
                WritingRun::Scheduled => SCHEDULED,
            }
            .into(),
            revision: database_revision(revision)?,
            resume: match resume {
                Resume::Auto => AUTO,
                Resume::Manual => MANUAL,
            }
            .into(),
            unclean: 0,
        };
        Ok(self.database.admit_run(&row, milliseconds(chain_cutoff))?)
    }

    /// The pair's admitted run ended as `end`.
    pub fn end_run(&mut self, id: PairId, end: RunEnd) -> Result<(), Error> {
        Ok(self.database.end_run(id.as_bytes(), end)?)
    }

    /// At an agent start, the runs the agent before it never saw end;
    /// `unclean` when that agent stopped uncleanly.
    pub fn interrupted_at_start(&mut self, unclean: bool) -> Result<Vec<InterruptedRun>, Error> {
        self.database
            .interrupted_at_start(unclean)?
            .into_iter()
            .map(interrupted)
            .collect()
    }

    /// A person chose not to continue the pair's interrupted run.
    pub fn forget_interrupted(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.forget_interrupted(id.as_bytes())?)
    }

    /// A job that is not admitted took the place of the pair's
    /// interrupted run: the next run still continues its journal, but
    /// nothing resumes it.
    pub fn end_interrupted(&mut self, id: PairId) -> Result<(), Error> {
        Ok(self.database.end_interrupted(id.as_bytes())?)
    }
}

fn interrupted(row: InterruptedRow) -> Result<InterruptedRun, Error> {
    let invalid = || Error::InvalidStored("interrupted run");
    let started = u64::try_from(row.started_ms).map_err(|_| invalid())?;
    Ok(InterruptedRun {
        pair: PairId::from_bytes(row.pair.as_slice().try_into().map_err(|_| invalid())?)
            .map_err(|_| invalid())?,
        started: UNIX_EPOCH
            .checked_add(Duration::from_millis(started))
            .ok_or_else(invalid)?,
        run: match row.trigger.as_str() {
            MANUAL => WritingRun::Manual,
            SCHEDULED => WritingRun::Scheduled,
            _ => return Err(invalid()),
        },
        revision: u64::try_from(row.revision).map_err(|_| invalid())?,
        resume: match row.resume.as_str() {
            AUTO => Resume::Auto,
            MANUAL => Resume::Manual,
            _ => return Err(invalid()),
        },
        unclean: u32::try_from(row.unclean).unwrap_or(u32::MAX),
    })
}

fn milliseconds(time: SystemTime) -> i64 {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use foldover_types::SyncMode;

    use super::super::tests::{draft, location};
    use super::*;

    #[test]
    fn interrupted_runs_round_trip_and_go_once_declined() {
        let (_directory, location) = location();
        let mut store = PairStore::open(&location).unwrap();
        let pair = store.create(&draft(SyncMode::Sync)).unwrap();
        let started = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        store
            .admit_run(
                pair.id(),
                started,
                WritingRun::Scheduled,
                pair.revision(),
                Resume::Auto,
                UNIX_EPOCH,
            )
            .unwrap();
        assert_eq!(
            store.interrupted_at_start(true).unwrap(),
            [InterruptedRun {
                pair: pair.id(),
                started,
                run: WritingRun::Scheduled,
                revision: pair.revision(),
                resume: Resume::Auto,
                unclean: 1,
            }]
        );
        store.forget_interrupted(pair.id()).unwrap();
        assert!(store.interrupted_at_start(false).unwrap().is_empty());

        store
            .admit_run(
                pair.id(),
                started,
                WritingRun::Manual,
                pair.revision(),
                Resume::Manual,
                UNIX_EPOCH,
            )
            .unwrap();
        store.end_run(pair.id(), RunEnd::Spent).unwrap();
        assert!(store.interrupted_at_start(false).unwrap().is_empty());
    }
}
