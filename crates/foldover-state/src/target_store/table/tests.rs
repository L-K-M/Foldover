use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Memory {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    fail_at: AtomicUsize,
    writes: AtomicUsize,
}
impl BlobStore for Memory {
    fn read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        let bytes = self.files.lock().unwrap().get(name).cloned();
        if bytes.as_ref().is_some_and(|v| v.len() > limit) {
            return Err(invalid("too large"));
        }
        Ok(bytes)
    }
    fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let write = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        if self.fail_at.load(Ordering::Relaxed) == write {
            return Err(io::Error::other("injected write failure"));
        }
        let mut files = self.files.lock().unwrap();
        if files.contains_key(name) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        files.insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }
}
fn entry(key: u32, value: Option<Vec<u8>>) -> Entry {
    Entry {
        key: key.to_be_bytes().to_vec(),
        value,
    }
}
fn values(root: &TableRoot, store: &Memory) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut result = BTreeMap::new();
    root.visit(store, &mut |key, value| {
        result.insert(key.to_vec(), value.to_vec());
        Ok(())
    })?;
    Ok(result)
}

#[test]
fn randomized_updates_match_map_including_duplicates_and_tombstones() {
    let store = Memory::default();
    let mut root = TableRoot::default();
    let mut expected = BTreeMap::new();
    let mut random = 47u64;
    for round in 0..180 {
        let mut batch = Vec::new();
        for _ in 0..37 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = ((random >> 32) % 700) as u32;
            let value = (random & 3 != 0).then(|| random.to_le_bytes().to_vec());
            if let Some(value) = &value {
                expected.insert(key.to_be_bytes().to_vec(), value.clone());
            } else {
                expected.remove(key.to_be_bytes().as_slice());
            }
            batch.push(entry(key, value));
        }
        root = root.update(&store, batch).unwrap();
        root = TableRoot::from_bytes(&root.to_bytes().unwrap()).unwrap();
        assert_eq!(values(&root, &store).unwrap(), expected, "round {round}");
    }
    let compact = root.compact(&store).unwrap();
    assert_eq!(values(&compact, &store).unwrap(), expected);
}

#[test]
fn bulk_import_streams_blocks_and_tiny_update_preserves_large_segment() {
    let store = Memory::default();
    let root = TableRoot::from_sorted(
        &store,
        (0..6000).map(|key| Ok(entry(key, Some(vec![key as u8; 900])))),
    )
    .unwrap();
    assert!(root.files().len() > 4);
    let before = root.files();
    let next = root
        .update(&store, [entry(1, None), entry(6001, Some(vec![4]))])
        .unwrap();
    assert!(before.iter().all(|file| next.files().contains(file)));
    let rows = values(&next, &store).unwrap();
    assert_eq!(rows.len(), 6000);
    assert!(!rows.contains_key(1u32.to_be_bytes().as_slice()));
    assert!(
        store
            .files
            .lock()
            .unwrap()
            .values()
            .all(|bytes| bytes.len() <= BLOCK_LIMIT)
    );
}

#[test]
fn missing_or_corrupt_blocks_are_errors_not_empty_baselines() {
    let store = Memory::default();
    let root = TableRoot::from_sorted(&store, [Ok(entry(1, Some(vec![2])))]).unwrap();
    let name = root.files().remove(0);
    let bytes = store.files.lock().unwrap().remove(&name).unwrap();
    assert!(values(&root, &store).is_err());
    let mut corrupt = bytes.clone();
    corrupt[8] ^= 1;
    store.files.lock().unwrap().insert(name.clone(), corrupt);
    assert!(values(&root, &store).is_err());
    // A content-address collision cannot be accepted just because it exists.
    assert!(TableRoot::from_sorted(&store, [Ok(entry(1, Some(vec![2])))]).is_err());
    store.files.lock().unwrap().insert(name, bytes);
    assert_eq!(values(&root, &store).unwrap().len(), 1);
}

#[test]
fn a_failed_update_leaves_the_previous_root_readable() {
    let store = Memory::default();
    let root = TableRoot::from_sorted(&store, [Ok(entry(1, Some(vec![2])))]).unwrap();
    store
        .fail_at
        .store(store.writes.load(Ordering::Relaxed) + 2, Ordering::Relaxed);
    assert!(root.update(&store, [entry(3, Some(vec![4]))]).is_err());
    assert_eq!(
        values(&root, &store).unwrap(),
        BTreeMap::from([(1u32.to_be_bytes().to_vec(), vec![2])])
    );
}

#[test]
fn parsers_and_writers_reject_invalid_limits_and_order() {
    let store = Memory::default();
    assert!(TableRoot::from_sorted(&store, [Ok(entry(2, None)), Ok(entry(1, None))]).is_err());
    assert!(TableRoot::from_sorted(&store, [Ok(entry(1, None)), Ok(entry(1, None))]).is_err());
    assert!(
        TableRoot::default()
            .update(&store, (0..4097).map(|k| entry(k, None)))
            .is_err()
    );
    assert!(
        TableRoot::default()
            .update(&store, [entry(1, Some(vec![0; ENTRY_LIMIT]))])
            .is_err()
    );
    let root = TableRoot::from_sorted(&store, [Ok(entry(1, Some(vec![2])))]).unwrap();
    let bytes = root.to_bytes().unwrap();
    for length in 0..bytes.len() {
        assert!(TableRoot::from_bytes(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(1);
    assert!(TableRoot::from_bytes(&trailing).is_err());
    let mut excess = ROOT_MAGIC.to_vec();
    excess.push(33);
    assert!(TableRoot::from_bytes(&excess).is_err());
}
