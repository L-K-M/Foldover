use super::*;
use std::cell::Cell;
use std::fs;

use foldover_types::{EntryContent, Propagation};
use proptest::prelude::*;

const ALL: MetadataPolicy =
    MetadataPolicy::new(Propagation::Sync, Propagation::Sync, Propagation::Sync);

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

fn entry() -> ObservedEntry {
    ObservedEntry::for_tests(
        EntryVersion::new(EntryContent::Directory, Digest::from_bytes([0; 32])),
        0,
    )
}

#[test]
fn cancellation_after_last_checked_entry_discards_observation() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("file"), b"content").unwrap();
    let endpoint = LocalEndpoint::open(temporary.path()).unwrap();
    let checked_count = Cell::new(0);

    let result = endpoint.observe_reporting(
        ALL,
        &Filter::none(),
        &BTreeMap::new(),
        &mut || {
            if checked_count.get() == 1 {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        },
        &mut |step| {
            if step == ObserveStep::Checked {
                checked_count.set(checked_count.get() + 1);
            }
        },
    );

    assert!(matches!(result, Err(ScanError::Cancelled)));
}

#[test]
fn unavailable_paths_preserve_byte_names_and_component_boundaries() {
    let roots = [path(b"a"), path(b"a/nested"), path(b"\xff/tree")];
    let unavailable = UnavailablePaths::from_paths(&roots);
    for (bytes, expected) in [
        (b"a".as_slice(), true),
        (b"a/child", true),
        (b"a/nested/child", true),
        (b"a!", false),
        (b"a!/child", false),
        (b"a-else/child", false),
        (b"ab/child", false),
        (b"\xff/tree", true),
        (b"\xff/tree/\xfe", true),
        (b"\xff/trees/\xfe", false),
        (b"\xfe/tree", false),
    ] {
        assert_eq!(unavailable.contains(&path(bytes)), expected, "{bytes:?}");
    }
    assert!(!UnavailablePaths::default().contains(&path(b"a/child")));
}

#[test]
fn late_failed_subtrees_remove_previously_observed_children() {
    let mut observation = Observation::default();
    for bytes in [
        b"a".as_slice(),
        b"a/child",
        b"a/child/grandchild",
        b"a!/safe",
    ] {
        observation.entries.insert(path(bytes), entry());
    }
    let mut unavailable = UnavailablePaths::default();
    unavailable
        .skip_error(&mut observation, ScanError::Changed(Some(path(b"a"))))
        .unwrap();
    unavailable
        .retain_entries(&mut observation.entries, &mut || ScanControl::Continue)
        .unwrap();

    assert_eq!(observation.unavailable(), &[path(b"a")]);
    assert_eq!(observation.issues()[0].path(), &path(b"a"));
    assert_eq!(
        observation.entries.keys().cloned().collect::<Vec<_>>(),
        [path(b"a!/safe")]
    );
    assert!(unavailable.contains(&path(b"a/next")));
    assert!(matches!(
        unavailable.skip_error(&mut observation, ScanError::Cancelled),
        Err(ScanError::Cancelled)
    ));
    assert_eq!(observation.unavailable(), &[path(b"a")]);
}

#[test]
fn cancellation_during_filtering_stops_lookups_and_returns_an_error() {
    let mut entries = (0..100)
        .map(|i| (path(format!("safe/{i:03}").as_bytes()), entry()))
        .collect();
    let unavailable = UnavailablePaths::from_paths(&[path(b"unread")]);
    let mut checkpoints = 0;
    let result = unavailable.retain_entries(&mut entries, &mut || {
        checkpoints += 1;
        if checkpoints == 3 {
            ScanControl::Cancel
        } else {
            ScanControl::Continue
        }
    });
    assert!(matches!(result, Err(ScanError::Cancelled)));
    assert_eq!(checkpoints, 3);
}

fn valid_path() -> impl Strategy<Value = SyncPath> {
    let component = prop::collection::vec(
        any::<u8>().prop_filter("ordinary name byte", |byte| !matches!(*byte, 0 | b'/')),
        1..12,
    )
    .prop_filter("not dot traversal", |name| name != b"." && name != b"..");
    prop::collection::vec(component, 1..6).prop_map(|parts| {
        let bytes = parts.join(&b'/');
        path(&bytes)
    })
}

proptest! {
    #[test]
    fn indexed_membership_matches_the_original_ancestry_rule(
        roots in prop::collection::vec(valid_path(), 0..30),
        queries in prop::collection::vec(valid_path(), 0..30),
    ) {
        let mut unavailable = UnavailablePaths::default();
        let mut inserted = Vec::new();
        for root in roots {
            unavailable.insert(&root);
            inserted.push(root.clone());
            let mut descendant = root.as_bytes().to_vec();
            descendant.extend_from_slice(b"/child");
            for query in queries.iter().chain([&root, &path(&descendant)]) {
                let expected = inserted.iter().any(|root: &SyncPath| {
                    root == query || root.is_ancestor_of(query)
                });
                prop_assert_eq!(unavailable.contains(query), expected);
            }
        }
    }
}

/// Run explicitly to compare the old linear predicate with the actual index.
#[test]
#[ignore = "lookup microbenchmark; run with --ignored --nocapture"]
fn unavailable_lookup_scale() {
    const COUNT: usize = 5_000;
    let roots: Vec<_> = (0..COUNT)
        .map(|i| path(format!("Library/Application Support/cache/unavailable-{i}").as_bytes()))
        .collect();
    let queries: Vec<_> = (0..COUNT)
        .map(|i| path(format!("Library/Application Support/cache/readable-{i}").as_bytes()))
        .collect();
    let started = std::time::Instant::now();
    let linear_matches = queries
        .iter()
        .filter(|query| {
            roots
                .iter()
                .any(|root| root == *query || root.is_ancestor_of(query))
        })
        .count();
    let linear = started.elapsed();
    let unavailable = UnavailablePaths::from_paths(&roots);
    let started = std::time::Instant::now();
    let indexed_matches = queries
        .iter()
        .filter(|query| unavailable.contains(query))
        .count();
    let indexed = started.elapsed();
    assert_eq!(linear_matches, 0);
    assert_eq!(indexed_matches, linear_matches);
    eprintln!("{COUNT} roots × {COUNT} queries: linear {linear:?}, indexed {indexed:?}");
}
