//! One run's lease on a root, so two Macs never write one folder at once
//! (PLAN.md §4, ADR 0032). The lease is `.foldover/lock`:
//!
//! ```text
//! foldover-lease 1
//! holder 6f1c…/2026-10-20T10-00-00.000Z   one run of one agent
//! label Studio                             shown as "Studio is syncing"
//! beat 17                                  moved by the holder as it works
//! ```
//!
//! A lease is dead when its beat has not moved across two reads one TTL
//! apart, timed on the reader's own clock. Taking a dead one over first
//! creates `.foldover/lock.claim` exclusively, then moves it over the lock;
//! whoever's record ends up in the lock wins, and the other backs off. A
//! claim is itself a lease, so one left by a reclaimer that died is
//! reclaimed by the same rule.

use std::cell::Cell;
use std::time::{Duration, Instant};

use foldover_types::ScanControl;

use crate::{ApplyError, Endpoint, ScanError};

const FORMAT: &str = "foldover-lease 1";
/// Shown for a lease in a format this version cannot read.
const UNKNOWN_HOLDER: &str = "Another Mac";
/// Longest lease file read.
pub(crate) const LEASE_LIMIT: usize = 4096;

/// The lock, or a claim on it while a dead lease is taken over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockFile {
    Lock,
    Claim,
}

impl LockFile {
    pub(crate) fn path(self) -> &'static [u8] {
        match self {
            Self::Lock => b".foldover/lock",
            Self::Claim => b".foldover/lock.claim",
        }
    }
}

/// Who holds a lease: one run of one agent, and the name people see.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeaseHolder {
    pub id: String,
    pub label: String,
}

/// How long leases live and how often they are checked.
#[derive(Clone, Copy, Debug)]
pub struct LeaseTiming {
    /// A beat that has not moved for this long is dead.
    pub ttl: Duration,
    /// How often a holder moves its beat.
    pub beat: Duration,
    /// How often a waiting run looks again.
    pub poll: Duration,
    /// How long a run waits for another Mac before giving up.
    pub wait: Duration,
}

impl LeaseTiming {
    /// Long enough that a single slow copy between beats never looks dead.
    pub const STANDARD: Self = Self {
        ttl: Duration::from_secs(120),
        beat: Duration::from_secs(15),
        poll: Duration::from_secs(2),
        wait: Duration::from_secs(150),
    };
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("{label} is syncing this folder")]
    Busy { label: String },
    #[error("Another Mac took over this folder while syncing; the sync stopped")]
    Lost,
    #[error("Waiting for another Mac was cancelled")]
    Cancelled,
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

impl From<ScanError> for LeaseError {
    fn from(error: ScanError) -> Self {
        Self::Apply(error.into())
    }
}

/// A lease on one root, held until dropped.
pub struct HeldLease<'a> {
    endpoint: &'a Endpoint,
    holder: LeaseHolder,
    renewed: Cell<Instant>,
    timing: LeaseTiming,
}

/// A lease file's content.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Record {
    holder: String,
    label: String,
    beat: u64,
}

impl Record {
    fn of(holder: &LeaseHolder, beat: u64) -> Self {
        Self {
            holder: holder.id.clone(),
            label: holder.label.clone(),
            beat,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        format!(
            "{FORMAT}\nholder {}\nlabel {}\nbeat {}\n",
            self.holder, self.label, self.beat
        )
        .into_bytes()
    }

    /// None for anything else, e.g. a lock another version wrote.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        (lines.next()? == FORMAT).then_some(())?;
        let holder = lines.next()?.strip_prefix("holder ")?.to_owned();
        let label = lines.next()?.strip_prefix("label ")?.to_owned();
        let beat = lines.next()?.strip_prefix("beat ")?.parse().ok()?;
        Some(Self {
            holder,
            label,
            beat,
        })
    }
}

/// A lease seen held by someone else, and since when unchanged.
struct Seen {
    record: Option<Record>,
    bytes: Vec<u8>,
    since: Instant,
}

impl Seen {
    /// The name of whoever holds the lease seen, as people see it.
    fn label(seen: Option<&Seen>) -> &str {
        seen.and_then(|seen| seen.record.as_ref())
            .map_or(UNKNOWN_HOLDER, |record| record.label.as_str())
    }

    /// Whether `bytes` stayed the same for a whole TTL; restarts the clock
    /// when they moved.
    fn dead(slot: &mut Option<Seen>, bytes: &[u8], ttl: Duration) -> bool {
        match slot {
            Some(seen) if seen.bytes == bytes => seen.since.elapsed() >= ttl,
            _ => {
                *slot = Some(Seen {
                    record: Record::parse(bytes),
                    bytes: bytes.to_vec(),
                    since: Instant::now(),
                });
                false
            }
        }
    }
}

impl<'a> HeldLease<'a> {
    /// Take `endpoint`'s lease for `holder`, waiting up to `timing.wait`
    /// while another live run holds it and taking over a dead one.
    /// `waiting` hears the holder's name once, when waiting starts.
    pub fn take(
        endpoint: &'a Endpoint,
        holder: LeaseHolder,
        timing: LeaseTiming,
        control: &mut impl FnMut() -> ScanControl,
        waiting: &mut impl FnMut(&str),
    ) -> Result<Self, LeaseError> {
        let deadline = Instant::now() + timing.wait;
        let (mut lock, mut claim): (Option<Seen>, Option<Seen>) = (None, None);
        let (mut announced, mut released) = (false, false);
        loop {
            let ours = Record::of(&holder, 0);
            if endpoint.create_lock_file(LockFile::Lock, &ours.bytes())? {
                return Ok(Self::held(endpoint, holder, timing));
            }
            match endpoint.read_lock_file(LockFile::Lock)? {
                // Released meanwhile: try again at once. A lock that keeps
                // vanishing as this run creates its own waits below like a
                // busy one, so it cannot spin past the deadline.
                None if !released => {
                    released = true;
                    continue;
                }
                None => released = false,
                Some(bytes) => {
                    released = false;
                    if Record::parse(&bytes).is_some_and(|record| record.holder == holder.id) {
                        return Ok(Self::held(endpoint, holder, timing));
                    }
                    // Observe both records together. A crash during a beat
                    // can leave both behind; starting the claim's TTL only
                    // after the lock's would exceed STANDARD's total wait on
                    // every attempt and make that folder unrecoverable.
                    match endpoint.read_lock_file(LockFile::Claim)? {
                        Some(bytes) => {
                            Seen::dead(&mut claim, &bytes, timing.ttl);
                        }
                        None => claim = None,
                    }
                    if Seen::dead(&mut lock, &bytes, timing.ttl)
                        && Self::take_over(endpoint, &holder, &bytes, &mut claim, timing.ttl)?
                    {
                        return Ok(Self::held(endpoint, holder, timing));
                    }
                }
            }
            let label = Seen::label(lock.as_ref());
            if Instant::now() >= deadline {
                return Err(LeaseError::Busy {
                    label: label.to_owned(),
                });
            }
            if !announced {
                waiting(label);
                announced = true;
            }
            // Only waiting asks; a free lease costs the run no check.
            if control() == ScanControl::Cancel {
                return Err(LeaseError::Cancelled);
            }
            std::thread::sleep(timing.poll);
        }
    }

    /// Take over the dead lease `dead` through an exclusive claim; whether
    /// this run won.
    fn take_over(
        endpoint: &Endpoint,
        holder: &LeaseHolder,
        dead: &[u8],
        claim: &mut Option<Seen>,
        ttl: Duration,
    ) -> Result<bool, LeaseError> {
        let ours = Record::of(holder, 0);
        if !endpoint.create_lock_file(LockFile::Claim, &ours.bytes())? {
            // Another Mac is taking over; a claim left by one that died is
            // removed once it too stays still for a TTL.
            if let Some(bytes) = endpoint.read_lock_file(LockFile::Claim)?
                && Seen::dead(claim, &bytes, ttl)
            {
                endpoint.remove_lock_file(LockFile::Claim)?;
            }
            return Ok(false);
        }
        if endpoint.read_lock_file(LockFile::Lock)?.as_deref() != Some(dead) {
            // The holder moved after all, or someone else already won.
            endpoint.remove_lock_file(LockFile::Claim)?;
            return Ok(false);
        }
        // Whoever's claim the move carries wins; a lost race backs off.
        let _ = endpoint.promote_claim();
        let current = endpoint.read_lock_file(LockFile::Lock)?;
        Ok(current
            .and_then(|bytes| Record::parse(&bytes))
            .is_some_and(|record| record.holder == holder.id))
    }

    fn held(endpoint: &'a Endpoint, holder: LeaseHolder, timing: LeaseTiming) -> Self {
        Self {
            endpoint,
            holder,
            renewed: Cell::new(Instant::now()),
            timing,
        }
    }

    /// Move the beat if one is due, after checking the lease is still this
    /// run's.
    pub fn renew_if_due(&self) -> Result<(), LeaseError> {
        if self.renewed.get().elapsed() < self.timing.beat {
            return Ok(());
        }
        self.renew()
    }

    /// Replace this run's exact lock through an exclusive claim. The claim
    /// serializes a beat or release with a concurrent dead-lock takeover.
    fn advance_through_claim(&self) -> Result<Option<u64>, LeaseError> {
        let Some(current) = self.endpoint.read_lock_file(LockFile::Lock)? else {
            return Ok(None);
        };
        let Some(record) = Record::parse(&current) else {
            return Ok(None);
        };
        if record.holder != self.holder.id {
            return Ok(None);
        }
        let Some(beat) = record.beat.checked_add(1) else {
            return Ok(None);
        };
        let next = Record::of(&self.holder, beat).bytes();
        let claimed = Instant::now();
        if !self.endpoint.create_lock_file(LockFile::Claim, &next)? {
            return Ok(None);
        }

        let unchanged = match self.endpoint.read_lock_file(LockFile::Lock) {
            Ok(lock) => lock.as_deref() == Some(current.as_slice()),
            Err(error) => {
                self.abandon_claim(claimed)?;
                return Err(error.into());
            }
        };
        if !unchanged {
            self.abandon_claim(claimed)?;
            return Ok(None);
        }

        // A failed promotion can still have reached the endpoint. Re-reading
        // the exact record decides the result without retrying a replacement.
        // Once promotion is attempted, never remove the claim path: another
        // reclaimer can create its own there as soon as ours moves.
        let promoted = self.endpoint.promote_claim();
        match self.endpoint.read_lock_file(LockFile::Lock) {
            Ok(Some(bytes)) if bytes == next && claimed.elapsed() < self.timing.ttl => {
                Ok(Some(beat))
            }
            Ok(Some(bytes)) if bytes == next => Ok(None),
            Ok(_) => promoted.map(|()| None).map_err(Into::into),
            Err(error) => match promoted {
                Ok(()) => Err(error.into()),
                Err(promote) => Err(promote.into()),
            },
        }
    }

    /// Remove the claim this call created while no stale-claim reclaimer can
    /// yet have replaced it. Once a TTL passes, recovery owns its cleanup.
    fn abandon_claim(&self, claimed: Instant) -> Result<(), LeaseError> {
        if claimed.elapsed() < self.timing.ttl {
            self.endpoint.remove_lock_file(LockFile::Claim)?;
        }
        Ok(())
    }

    /// Move the beat after fencing it against a concurrent takeover.
    pub fn renew(&self) -> Result<(), LeaseError> {
        if self.advance_through_claim()?.is_none() {
            return Err(LeaseError::Lost);
        }
        self.renewed.set(Instant::now());
        Ok(())
    }
}

impl Drop for HeldLease<'_> {
    fn drop(&mut self) {
        // Promote our claim before removal. A crash then leaves an ordinary
        // advanced lease, never a free lock beside a stale claim.
        if matches!(self.advance_through_claim(), Ok(Some(_))) {
            let _ = self.endpoint.remove_lock_file(LockFile::Lock);
        }
    }
}

#[cfg(test)]
mod tests;
