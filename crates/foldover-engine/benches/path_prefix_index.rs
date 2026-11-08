//! Criterion benchmark for `PathPrefixIndex` at one million queries
//! against ten thousand roots (A5 scaling qualification). Two path
//! shapes share the setup: a shallow two-component root with a
//! three-component query, and a deep five-component root with a
//! nine-component query. Both use Unicode names as raw UTF-8 bytes.
//!
//! Half the queries are descendants of a root and must match; half are
//! boundary siblings that share leading bytes with a root component but
//! diverge before a `/`, so they must not match. The expected hit count
//! is asserted outside the measured loop.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use foldover_types::{PathPrefixIndex, SyncPath};

const ROOTS: usize = 10_000;
const QUERIES: usize = 1_000_000;
const EXPECTED_HITS: usize = QUERIES / 2;

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

/// Count ancestor hits without timing; the assertion uses this before
/// criterion measures the same loop.
fn count_hits(index: &PathPrefixIndex, queries: &[SyncPath]) -> usize {
    let mut hits = 0;
    for query in queries {
        hits += usize::from(index.contains_ancestor(query));
    }
    hits
}

/// Shallow roots: `racine-00000/café`, two components with one Unicode
/// name. Descendants append `/fichier-0000000`; boundary siblings
/// replace `café` with `café-frère`, which shares the raw bytes of the
/// root's second component but is a different component.
fn shallow_roots() -> Vec<SyncPath> {
    (0..ROOTS)
        .map(|index| path(format!("racine-{index:05}/café").as_bytes()))
        .collect()
}

fn shallow_queries() -> Vec<SyncPath> {
    (0..QUERIES)
        .map(|query| {
            let root = query % ROOTS;
            if query % 2 == 0 {
                path(format!("racine-{root:05}/café/fichier-{query:07}").as_bytes())
            } else {
                path(format!("racine-{root:05}/café-frère/fichier-{query:07}").as_bytes())
            }
        })
        .collect()
}

/// Deep roots: five components, three of them Unicode. Descendants
/// append four components; boundary siblings replace `分支` with
/// `分支-sœur` and keep the same depth, so the first divergence is a
/// shared multi-byte prefix four components down.
fn deep_roots() -> Vec<SyncPath> {
    (0..ROOTS)
        .map(|index| path(format!("racine-{index:05}/café/データ/分支/leaf").as_bytes()))
        .collect()
}

fn deep_queries() -> Vec<SyncPath> {
    (0..QUERIES)
        .map(|query| {
            let root = query % ROOTS;
            if query % 2 == 0 {
                path(
                    format!(
                        "racine-{root:05}/café/データ/分支/leaf/feuille-{query:07}/nested-1/nested-2/nested-3"
                    )
                    .as_bytes(),
                )
            } else {
                path(
                    format!(
                        "racine-{root:05}/café/データ/分支-sœur/feuille-{query:07}/nested-1/nested-2/nested-3/nested-4"
                    )
                    .as_bytes(),
                )
            }
        })
        .collect()
}

fn prefix_index_shallow_unicode_1m_queries_10k_roots(c: &mut Criterion) {
    let index = PathPrefixIndex::from_paths(&shallow_roots());
    let queries = shallow_queries();
    assert_eq!(index.len(), ROOTS);
    assert_eq!(queries.len(), QUERIES);
    assert_eq!(count_hits(&index, &queries), EXPECTED_HITS);
    c.bench_function("prefix_index_shallow_unicode_1m_queries_10k_roots", |b| {
        b.iter(|| {
            let mut hits = 0;
            for query in black_box(&queries) {
                hits += usize::from(black_box(index.contains_ancestor(query)));
            }
            black_box(hits)
        })
    });
}

fn prefix_index_deep_unicode_1m_queries_10k_roots(c: &mut Criterion) {
    let index = PathPrefixIndex::from_paths(&deep_roots());
    let queries = deep_queries();
    assert_eq!(index.len(), ROOTS);
    assert_eq!(queries.len(), QUERIES);
    assert_eq!(count_hits(&index, &queries), EXPECTED_HITS);
    c.bench_function("prefix_index_deep_unicode_1m_queries_10k_roots", |b| {
        b.iter(|| {
            let mut hits = 0;
            for query in black_box(&queries) {
                hits += usize::from(black_box(index.contains_ancestor(query)));
            }
            black_box(hits)
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = prefix_index_shallow_unicode_1m_queries_10k_roots,
        prefix_index_deep_unicode_1m_queries_10k_roots
}
criterion_main!(benches);
