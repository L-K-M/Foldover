use std::fs;

use foldover_types::PairRoot;

use super::*;
use crate::Reach;

/// Short enough for tests; the order of the parts is what matters.
const QUICK: LeaseTiming = LeaseTiming {
    ttl: Duration::from_millis(150),
    beat: Duration::from_millis(10),
    poll: Duration::from_millis(20),
    wait: Duration::from_secs(5),
};
const NO_WAIT: LeaseTiming = LeaseTiming {
    wait: Duration::ZERO,
    ..QUICK
};

/// Two agents' views of one folder.
fn folder() -> (tempfile::TempDir, Endpoint, Endpoint) {
    let directory = tempfile::tempdir().unwrap();
    let root =
        PairRoot::from_bytes(directory.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let open = || Endpoint::open(&root, &Reach::default()).unwrap();
    let (a, b) = (open(), open());
    (directory, a, b)
}

fn holder(id: &str, label: &str) -> LeaseHolder {
    LeaseHolder {
        id: id.into(),
        label: label.into(),
    }
}

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn take<'a>(
    endpoint: &'a Endpoint,
    holder: LeaseHolder,
    timing: LeaseTiming,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<HeldLease<'a>, LeaseError> {
    HeldLease::take(endpoint, holder, timing, control, &mut |_| {})
}

#[test]
fn a_held_lease_makes_another_run_wait_until_it_is_released() {
    let (_directory, a, b) = folder();
    let held = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed).unwrap();
    held.renew().unwrap();
    let busy = take(&b, holder("b/run", "Studio"), NO_WAIT, &mut proceed);
    assert!(matches!(busy, Err(LeaseError::Busy { label }) if label == "Laptop"));

    drop(held);
    let taken = take(&b, holder("b/run", "Studio"), NO_WAIT, &mut proceed);
    assert!(taken.is_ok());
}

#[test]
fn a_dead_lease_is_taken_over_and_its_holder_learns_it_lost() {
    let (directory, a, b) = folder();
    let stale = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed).unwrap();
    // A stops beating; B waits a TTL with the beat unmoved, then takes over.
    let taken = take(&b, holder("b/run", "Studio"), QUICK, &mut proceed).unwrap();
    assert!(matches!(stale.renew(), Err(LeaseError::Lost)));
    // The old holder's release leaves the new lease alone.
    drop(stale);
    let lock = fs::read_to_string(directory.path().join(".foldover/lock")).unwrap();
    assert!(lock.contains("holder b/run"), "{lock}");
    assert!(!directory.path().join(".foldover/lock.claim").exists());
    taken.renew().unwrap();
}

#[test]
fn a_live_holder_keeps_its_lease_while_beating() {
    let (_directory, a, b) = folder();
    let held = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed).unwrap();
    // A TTL well past a busy machine's stalls between beats, and a wait
    // long enough to take over a holder that stopped beating.
    let patient = LeaseTiming {
        ttl: Duration::from_secs(1),
        wait: Duration::from_millis(2_500),
        ..QUICK
    };
    let waiting = std::thread::scope(|scope| {
        let waiting =
            scope.spawn(move || take(&b, holder("b/run", "Studio"), patient, &mut proceed).is_ok());
        // The holder beats from its own thread, as a run does, until the
        // other gives up.
        while !waiting.is_finished() {
            held.renew().unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        waiting.join().unwrap()
    });
    assert!(!waiting, "a live lease was taken over");
}

#[test]
fn a_claim_left_by_a_reclaimer_that_died_is_cleared_in_turn() {
    let (directory, a, b) = folder();
    let _dead = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed).unwrap();
    // A third Mac died halfway through a takeover.
    fs::write(
        directory.path().join(".foldover/lock.claim"),
        Record::of(&holder("c/run", "Mini"), 0).bytes(),
    )
    .unwrap();
    let taken = take(&b, holder("b/run", "Studio"), QUICK, &mut proceed);
    assert!(taken.is_ok());
}

#[test]
fn a_lock_another_version_wrote_is_honoured_until_it_goes_still() {
    let (directory, a, _) = folder();
    fs::create_dir_all(directory.path().join(".foldover")).unwrap();
    fs::write(directory.path().join(".foldover/lock"), b"lease v9\n").unwrap();
    let busy = take(&a, holder("a/run", "Laptop"), NO_WAIT, &mut proceed);
    assert!(matches!(busy, Err(LeaseError::Busy { label }) if label == "Another Mac"));
    let taken = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed);
    assert!(taken.is_ok());
}

#[test]
fn waiting_stops_when_the_run_is_cancelled() {
    let (_directory, a, b) = folder();
    let _held = take(&a, holder("a/run", "Laptop"), QUICK, &mut proceed).unwrap();
    let mut heard = Vec::new();
    let cancelled = HeldLease::take(
        &b,
        holder("b/run", "Studio"),
        QUICK,
        &mut || ScanControl::Cancel,
        &mut |label| heard.push(label.to_owned()),
    );
    assert!(matches!(cancelled, Err(LeaseError::Cancelled)));
    // The run said whom it waited for before it stopped.
    assert_eq!(heard, ["Laptop"]);
}
