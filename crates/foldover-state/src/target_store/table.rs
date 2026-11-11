//! Immutable binary-level merge tables. Readers hold one bounded block per
//! occupied level; a failed writer never changes a previously published root.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::io;

use super::BlobStore;

const BLOCK_LIMIT: usize = 1024 * 1024;
const ENTRY_LIMIT: usize = 256 * 1024;
const UPDATE_LIMIT: usize = 4096;
const LEVEL_LIMIT: usize = 32;
const REFERENCE_LIMIT: usize = 100_000;
const ROOT_LIMIT: usize = 8 * 1024 * 1024;
const BLOCK_MAGIC: &[u8] = b"FTB1";
const ROOT_MAGIC: &[u8] = b"FTR1";
const TOMBSTONE: u32 = u32::MAX;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Block {
    digest: [u8; 32],
    bytes: u32,
    rows: u32,
}

impl Block {
    fn filename(&self) -> String {
        format!("{}.block", blake3::Hash::from_bytes(self.digest).to_hex())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Segment {
    blocks: Vec<Block>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableRoot {
    levels: Vec<Option<Segment>>,
}

impl TableRoot {
    pub fn from_sorted(
        store: &dyn BlobStore,
        entries: impl IntoIterator<Item = io::Result<Entry>>,
    ) -> io::Result<Self> {
        Self::from_visitor(store, |emit| {
            for entry in entries {
                emit(entry?)?;
            }
            Ok(())
        })
    }

    /// Callback form for streaming a pinned SQLite cursor without a channel,
    /// another thread, or a whole-table intermediate collection.
    pub fn from_visitor(
        store: &dyn BlobStore,
        visit: impl FnOnce(&mut dyn FnMut(Entry) -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut writer = Writer::new(store);
        visit(&mut |entry| writer.push(entry))?;
        let rows = writer.rows;
        let segment = writer.finish()?;
        if rows == 0 {
            return Ok(Self::default());
        }
        let batches = rows.div_ceil(256);
        let level = (u64::BITS - (batches - 1).leading_zeros()) as usize;
        if level >= LEVEL_LIMIT {
            return Err(invalid("Target table has too many rows"));
        }
        let mut levels = vec![None; level + 1];
        levels[level] = Some(segment);
        Ok(Self { levels })
    }

    pub fn update(
        &self,
        store: &dyn BlobStore,
        entries: impl IntoIterator<Item = Entry>,
    ) -> io::Result<Self> {
        let mut sorted = BTreeMap::new();
        for (index, entry) in entries.into_iter().enumerate() {
            if index >= UPDATE_LIMIT {
                return Err(invalid("Target table update exceeds batch limit"));
            }
            validate_entry(&entry)?;
            sorted.insert(entry.key, entry.value);
        }
        if sorted.is_empty() {
            return Ok(self.clone());
        }
        let mut writer = Writer::new(store);
        for (key, value) in sorted {
            writer.push(Entry { key, value })?;
        }
        let mut carry = writer.finish()?;
        let mut next = self.clone();
        for level in 0..LEVEL_LIMIT {
            if level == next.levels.len() {
                next.levels.push(None);
            }
            let Some(older) = next.levels[level].take() else {
                next.levels[level] = Some(carry);
                next.check_references()?;
                return Ok(next);
            };
            let mut writer = Writer::new(store);
            merge(store, [&carry, &older].into_iter(), &mut |entry| {
                writer.push(entry)
            })?;
            carry = writer.finish()?;
        }
        Err(invalid("Target table exhausted its merge levels"))
    }

    pub fn visit<F>(&self, store: &dyn BlobStore, visit: &mut F) -> io::Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> io::Result<()> + ?Sized,
    {
        merge(
            store,
            self.levels.iter().filter_map(Option::as_ref),
            &mut |entry| {
                if let Some(value) = entry.value {
                    visit(&entry.key, &value)?;
                }
                Ok(())
            },
        )
    }

    pub fn compact(&self, store: &dyn BlobStore) -> io::Result<Self> {
        Self::from_visitor(store, |emit| {
            self.visit(store, &mut |key, value| {
                emit(Entry {
                    key: key.to_vec(),
                    value: Some(value.to_vec()),
                })
            })
        })
    }

    pub fn files(&self) -> Vec<String> {
        self.levels
            .iter()
            .flatten()
            .flat_map(|segment| segment.blocks.iter().map(Block::filename))
            .collect()
    }

    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        self.check_references()?;
        let mut bytes = ROOT_MAGIC.to_vec();
        bytes.push(self.levels.len() as u8);
        for segment in &self.levels {
            let count = segment.as_ref().map_or(0, |s| s.blocks.len());
            bytes.extend((count as u32).to_le_bytes());
            if let Some(segment) = segment {
                for block in &segment.blocks {
                    bytes.extend(block.digest);
                    bytes.extend(block.bytes.to_le_bytes());
                    bytes.extend(block.rows.to_le_bytes());
                }
            }
        }
        if bytes.len() > ROOT_LIMIT {
            return Err(invalid("Target table root exceeds size limit"));
        }
        Ok(bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > ROOT_LIMIT {
            return Err(invalid("Target table root exceeds size limit"));
        }
        let mut input = Reader(bytes);
        if input.take(4)? != ROOT_MAGIC {
            return Err(invalid("Unsupported target table root"));
        }
        let count = input.take(1)?[0] as usize;
        if count > LEVEL_LIMIT {
            return Err(invalid("Too many target table levels"));
        }
        let mut levels = Vec::with_capacity(count);
        let mut references = 0;
        for _ in 0..count {
            let count = input.number()? as usize;
            references += count;
            if references > REFERENCE_LIMIT {
                return Err(invalid("Too many target table blocks"));
            }
            if count == 0 {
                levels.push(None);
                continue;
            }
            let mut blocks = Vec::with_capacity(count.min(input.0.len() / 40));
            for _ in 0..count {
                let digest = input.take(32)?.try_into().expect("checked size");
                let bytes = input.number()?;
                let rows = input.number()?;
                if !(17..=BLOCK_LIMIT as u32).contains(&bytes)
                    || rows == 0
                    || rows > (bytes - 8) / 9
                {
                    return Err(invalid("Invalid target block descriptor"));
                }
                blocks.push(Block {
                    digest,
                    bytes,
                    rows,
                });
            }
            levels.push(Some(Segment { blocks }));
        }
        input.end()?;
        if levels.last().is_some_and(Option::is_none) {
            return Err(invalid("Noncanonical target table levels"));
        }
        Ok(Self { levels })
    }

    fn check_references(&self) -> io::Result<()> {
        let count: usize = self.levels.iter().flatten().map(|s| s.blocks.len()).sum();
        if self.levels.len() > LEVEL_LIMIT || count > REFERENCE_LIMIT {
            return Err(invalid("Target table exceeds reference limit"));
        }
        Ok(())
    }
}

struct Writer<'a> {
    store: &'a dyn BlobStore,
    segment: Segment,
    bytes: Vec<u8>,
    block_rows: u32,
    rows: u64,
    last: Option<Vec<u8>>,
}

impl<'a> Writer<'a> {
    fn new(store: &'a dyn BlobStore) -> Self {
        Self {
            store,
            segment: Segment::default(),
            bytes: Vec::new(),
            block_rows: 0,
            rows: 0,
            last: None,
        }
    }

    fn push(&mut self, entry: Entry) -> io::Result<()> {
        validate_entry(&entry)?;
        if self.last.as_ref().is_some_and(|last| last >= &entry.key) {
            return Err(invalid("Target table entries are not strictly sorted"));
        }
        let size = 8 + entry.key.len() + entry.value.as_ref().map_or(0, Vec::len);
        if !self.bytes.is_empty() && self.bytes.len() + size > BLOCK_LIMIT {
            self.flush()?;
        }
        if self.bytes.is_empty() {
            self.bytes.extend(BLOCK_MAGIC);
            self.bytes.extend([0; 4]);
        }
        self.bytes.extend((entry.key.len() as u32).to_le_bytes());
        self.bytes.extend(
            entry
                .value
                .as_ref()
                .map_or(TOMBSTONE, |v| v.len() as u32)
                .to_le_bytes(),
        );
        self.bytes.extend(&entry.key);
        if let Some(value) = entry.value {
            self.bytes.extend(value);
        }
        self.block_rows += 1;
        self.rows += 1;
        self.last = Some(entry.key);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.block_rows == 0 {
            return Ok(());
        }
        if self.segment.blocks.len() >= REFERENCE_LIMIT {
            return Err(invalid("Too many target table blocks"));
        }
        self.bytes[4..8].copy_from_slice(&self.block_rows.to_le_bytes());
        let block = Block {
            digest: *blake3::hash(&self.bytes).as_bytes(),
            bytes: self.bytes.len() as u32,
            rows: self.block_rows,
        };
        let name = block.filename();
        match self.store.write_new(&name, &self.bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if self.store.read(&name, BLOCK_LIMIT)?.as_deref() != Some(self.bytes.as_slice()) {
                    return Err(invalid("Existing target block differs from its digest"));
                }
            }
            Err(error) => return Err(error),
        }
        self.segment.blocks.push(block);
        self.bytes.clear();
        self.block_rows = 0;
        Ok(())
    }

    fn finish(mut self) -> io::Result<Segment> {
        self.flush()?;
        Ok(self.segment)
    }
}

struct Cursor<'a> {
    store: &'a dyn BlobStore,
    blocks: std::slice::Iter<'a, Block>,
    entries: std::vec::IntoIter<Entry>,
    last: Option<Vec<u8>>,
}

impl<'a> Cursor<'a> {
    fn new(store: &'a dyn BlobStore, segment: &'a Segment) -> Self {
        Self {
            store,
            blocks: segment.blocks.iter(),
            entries: Vec::new().into_iter(),
            last: None,
        }
    }
    fn next(&mut self) -> io::Result<Option<Entry>> {
        if self.entries.len() == 0 {
            let Some(block) = self.blocks.next() else {
                return Ok(None);
            };
            self.entries = read_block(self.store, block)?.into_iter();
        }
        let entry = self
            .entries
            .next()
            .ok_or_else(|| invalid("Empty target block"))?;
        if self.last.as_ref().is_some_and(|last| last >= &entry.key) {
            return Err(invalid("Target blocks are not strictly sorted"));
        }
        self.last = Some(entry.key.clone());
        Ok(Some(entry))
    }
}

/// Earlier segments are newer. Equal keys are consumed together, including
/// tombstones, so an older value can never reappear after a delete.
fn merge<'a>(
    store: &'a dyn BlobStore,
    segments: impl Iterator<Item = &'a Segment>,
    emit: &mut dyn FnMut(Entry) -> io::Result<()>,
) -> io::Result<()> {
    let mut cursors: Vec<_> = segments
        .map(|segment| Cursor::new(store, segment))
        .collect();
    let mut heads = Vec::with_capacity(cursors.len());
    let mut heap = BinaryHeap::new();
    for (index, cursor) in cursors.iter_mut().enumerate() {
        let entry = cursor.next()?;
        if let Some(entry) = &entry {
            heap.push(Reverse((entry.key.clone(), index)));
        }
        heads.push(entry);
    }
    while let Some(Reverse((key, index))) = heap.pop() {
        let newest = heads[index].take().expect("heap has a head");
        advance(index, &mut cursors, &mut heads, &mut heap)?;
        while heap.peek().is_some_and(|Reverse((next, _))| next == &key) {
            let Reverse((_, older)) = heap.pop().expect("peeked head");
            heads[older].take();
            advance(older, &mut cursors, &mut heads, &mut heap)?;
        }
        emit(newest)?;
    }
    Ok(())
}

fn advance(
    index: usize,
    cursors: &mut [Cursor<'_>],
    heads: &mut [Option<Entry>],
    heap: &mut BinaryHeap<Reverse<(Vec<u8>, usize)>>,
) -> io::Result<()> {
    heads[index] = cursors[index].next()?;
    if let Some(entry) = &heads[index] {
        heap.push(Reverse((entry.key.clone(), index)));
    }
    Ok(())
}

fn read_block(store: &dyn BlobStore, block: &Block) -> io::Result<Vec<Entry>> {
    let bytes = store
        .read(&block.filename(), BLOCK_LIMIT)?
        .ok_or_else(|| invalid("Missing target table block"))?;
    if bytes.len() != block.bytes as usize || blake3::hash(&bytes).as_bytes() != &block.digest {
        return Err(invalid("Target table block checksum mismatch"));
    }
    let mut input = Reader(&bytes);
    if input.take(4)? != BLOCK_MAGIC || input.number()? != block.rows {
        return Err(invalid("Invalid target block header"));
    }
    let mut entries = Vec::with_capacity(block.rows as usize);
    for _ in 0..block.rows {
        let key_length = input.number()? as usize;
        let value_length = input.number()?;
        let length = key_length
            .checked_add(if value_length == TOMBSTONE {
                0
            } else {
                value_length as usize
            })
            .ok_or_else(|| invalid("Target entry length overflow"))?;
        if key_length == 0 || length > ENTRY_LIMIT {
            return Err(invalid("Invalid target entry length"));
        }
        let key = input.take(key_length)?.to_vec();
        let value = if value_length == TOMBSTONE {
            None
        } else {
            Some(input.take(value_length as usize)?.to_vec())
        };
        entries.push(Entry { key, value });
    }
    input.end()?;
    Ok(entries)
}

fn validate_entry(entry: &Entry) -> io::Result<()> {
    if entry.key.is_empty()
        || entry
            .key
            .len()
            .saturating_add(entry.value.as_ref().map_or(0, Vec::len))
            > ENTRY_LIMIT
    {
        return Err(invalid("Target table entry exceeds size limit"));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        if count > self.0.len() {
            return Err(invalid("Truncated target table"));
        }
        let (bytes, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(bytes)
    }
    fn number(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("checked size"),
        ))
    }
    fn end(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("Trailing target table bytes"))
        }
    }
}

#[cfg(test)]
mod tests;
