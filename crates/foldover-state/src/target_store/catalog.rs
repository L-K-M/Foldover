//! Versioned checkpoints bind all of a pair's trees to one publication.
//! Immutable blocks may arrive in any order; only a complete checkpoint
//! makes them authoritative. Checksums detect corruption, not malicious edits.

use std::io;

use foldover_types::{BaselineEntry, EndpointId, PairId, Side, SyncPath};

use super::{BlobStore, Entry, TableRoot};
use crate::base::{VERSION_BYTES, decode, decode_entry, encode};
use crate::{BaseUpdate, Journal, JournalAction, JournalEntry, JournalUpdate, SnapshotEntry};

pub const CHECKPOINT_LIMIT: usize = 16 * 1024 * 1024;
const MAGIC: &[u8] = b"foldover-target-state\0\x01";
const TEXT_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub epoch: [u8; 16],
    pub generation: u64,
    pub digest: [u8; 32],
}

impl Checkpoint {
    pub fn filename(&self) -> String {
        format!(
            "c-{}-{:016x}-{}",
            hex(&self.epoch),
            self.generation,
            hex(&self.digest)
        )
    }

    pub fn parse_filename(name: &str) -> Option<Self> {
        let mut fields = name.strip_prefix("c-")?.split('-');
        let epoch = unhex(fields.next()?)?;
        let generation = fields.next()?;
        if generation.len() != 16 {
            return None;
        }
        let generation = u64::from_str_radix(generation, 16).ok()?;
        let digest = unhex(fields.next()?)?;
        let result = Self {
            epoch,
            generation,
            digest,
        };
        (fields.next().is_none() && generation > 0 && result.filename() == name).then_some(result)
    }
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub pair: PairId,
    pub target: EndpointId,
    pub checkpoint: Checkpoint,
    pub previous: [u8; 32],
    pub description: String,
    pub base: TableRoot,
    pub snapshots: [TableRoot; 2],
    pub journal: TableRoot,
    pub run: Option<String>,
    pub spent: Vec<SyncPath>,
}

impl Catalog {
    pub fn empty(
        pair: PairId,
        target: EndpointId,
        checkpoint: Checkpoint,
        description: String,
    ) -> Self {
        Self {
            pair,
            target,
            checkpoint,
            previous: [0; 32],
            description,
            base: TableRoot::default(),
            snapshots: [TableRoot::default(), TableRoot::default()],
            journal: TableRoot::default(),
            run: None,
            spent: Vec::new(),
        }
    }

    /// Serialize the next immutable commit, without changing the published root.
    pub fn next(&self) -> io::Result<(Self, Vec<u8>)> {
        let mut next = self.clone();
        next.previous = self.checkpoint.digest;
        next.checkpoint.generation = self
            .checkpoint
            .generation
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| invalid("Catalog generation exhausted"))?;
        let bytes = next.encode()?;
        next.checkpoint.digest = *blake3::hash(&bytes).as_bytes();
        Ok((next, bytes))
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let mut out = MAGIC.to_vec();
        out.extend(self.pair.as_bytes());
        out.extend(self.target.as_bytes());
        out.extend(self.checkpoint.epoch);
        out.extend(self.checkpoint.generation.to_le_bytes());
        out.extend(self.previous);
        put(&mut out, self.description.as_bytes(), TEXT_LIMIT)?;
        put(
            &mut out,
            self.run.as_deref().unwrap_or_default().as_bytes(),
            TEXT_LIMIT,
        )?;
        if self.spent.len() > 4096 {
            return Err(invalid("Too many spent conflict choices"));
        }
        out.extend((self.spent.len() as u32).to_le_bytes());
        for path in &self.spent {
            put(&mut out, path.as_bytes(), TEXT_LIMIT)?;
        }
        for table in [
            &self.base,
            &self.snapshots[0],
            &self.snapshots[1],
            &self.journal,
        ] {
            put(&mut out, &table.to_bytes()?, CHECKPOINT_LIMIT)?;
        }
        if out.len() > CHECKPOINT_LIMIT {
            return Err(invalid("Target checkpoint is too large"));
        }
        Ok(out)
    }

    pub fn decode(checkpoint: Checkpoint, bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > CHECKPOINT_LIMIT || blake3::hash(bytes).as_bytes() != &checkpoint.digest {
            return Err(invalid("Target checkpoint checksum mismatch"));
        }
        let mut input = Reader(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(invalid("Unsupported target catalog format"));
        }
        let pair = PairId::from_bytes(input.array()?).map_err(other)?;
        let target = EndpointId::from_bytes(input.array()?);
        if input.array::<16>()? != checkpoint.epoch
            || u64::from_le_bytes(input.array()?) != checkpoint.generation
        {
            return Err(invalid("Target checkpoint identity mismatch"));
        }
        let previous = input.array()?;
        let description = input.text()?;
        let run = input.text()?;
        let count = u32::from_le_bytes(input.array()?) as usize;
        if count > 4096 {
            return Err(invalid("Too many spent conflict choices"));
        }
        let mut spent = Vec::with_capacity(count);
        for _ in 0..count {
            spent.push(SyncPath::try_from(input.bytes(TEXT_LIMIT)?).map_err(other)?);
        }
        let mut table = || TableRoot::from_bytes(input.bytes(CHECKPOINT_LIMIT)?);
        let base = table()?;
        let snapshots = [table()?, table()?];
        let journal = table()?;
        input.end()?;
        Ok(Self {
            pair,
            target,
            checkpoint,
            previous,
            description,
            base,
            snapshots,
            journal,
            run: (!run.is_empty()).then_some(run),
            spent,
        })
    }

    pub fn visit_base(
        &self,
        store: &dyn BlobStore,
        visit: &mut dyn FnMut(SyncPath, BaselineEntry),
    ) -> io::Result<()> {
        self.base.visit(store, &mut |key, value| {
            if value.len() != VERSION_BYTES * 2 {
                return Err(invalid("Invalid target baseline"));
            }
            let path = SyncPath::try_from(key).map_err(other)?;
            let entry =
                decode_entry(&value[..VERSION_BYTES], &value[VERSION_BYTES..]).map_err(other)?;
            visit(path, entry);
            Ok(())
        })
    }

    pub fn visit_snapshot(
        &self,
        store: &dyn BlobStore,
        side: Side,
        visit: &mut dyn FnMut(SnapshotEntry),
    ) -> io::Result<()> {
        self.snapshots[index(side)].visit(store, &mut |key, value| {
            if value.len() < VERSION_BYTES {
                return Err(invalid("Invalid target snapshot"));
            }
            visit(SnapshotEntry {
                path: SyncPath::try_from(key).map_err(other)?,
                version: decode(&value[..VERSION_BYTES]).map_err(other)?,
                fingerprint: value[VERSION_BYTES..].to_vec(),
            });
            Ok(())
        })
    }

    pub fn read_journal(&self, store: &dyn BlobStore) -> io::Result<Option<Journal>> {
        let Some(run) = &self.run else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        self.journal.visit(store, &mut |key, value| {
            entries.push(decode_journal(key, value)?);
            Ok(())
        })?;
        Ok((!entries.is_empty()).then(|| Journal {
            run: run.clone(),
            entries,
        }))
    }

    /// Updating a clone preserves the old committed roots if any write fails.
    pub fn change(
        &self,
        store: &dyn BlobStore,
        updates: &[BaseUpdate],
        journal: JournalUpdate<'_>,
    ) -> io::Result<Self> {
        let mut next = self.clone();
        next.spent = updates.iter().map(|update| update.path().clone()).collect();
        if !updates.is_empty() {
            next.base = self.base.update(
                store,
                updates.iter().map(|update| match update {
                    BaseUpdate::Record(path, entry) => base_entry(path, *entry),
                    BaseUpdate::Forget(path) => Entry {
                        key: path.as_bytes().to_vec(),
                        value: None,
                    },
                }),
            )?;
        }
        match journal {
            JournalUpdate::None | JournalUpdate::Hold => {}
            JournalUpdate::Record { run, entries } if !entries.is_empty() => {
                if run.is_empty() || run.len() > TEXT_LIMIT {
                    return Err(invalid("Invalid target journal run"));
                }
                if next.run.as_deref() != Some(run) {
                    next.journal = TableRoot::default();
                }
                next.run = Some(run.to_owned());
                let rows = entries
                    .iter()
                    .map(journal_entry)
                    .collect::<io::Result<Vec<_>>>()?;
                next.journal = next.journal.update(store, rows)?;
            }
            JournalUpdate::Record { .. } => {}
            JournalUpdate::Undid { run, seqs } => {
                if next.run.as_deref() != Some(run) {
                    return Err(invalid("Target undo journal changed"));
                }
                next.journal = next.journal.update(
                    store,
                    seqs.iter().map(|seq| Entry {
                        key: seq.to_be_bytes().to_vec(),
                        value: None,
                    }),
                )?;
            }
            JournalUpdate::Spend { run } => {
                if next.run.as_deref() != Some(run) {
                    return Err(invalid("Target undo journal changed"));
                }
                next.run = None;
                next.journal = TableRoot::default();
            }
        }
        Ok(next)
    }

    pub fn replace_snapshot(
        &self,
        store: &dyn BlobStore,
        side: Side,
        entries: impl IntoIterator<Item = SnapshotEntry>,
    ) -> io::Result<Self> {
        let mut next = self.clone();
        next.spent.clear();
        next.snapshots[index(side)] = TableRoot::from_sorted(
            store,
            entries.into_iter().map(|entry| Ok(snapshot_entry(&entry))),
        )?;
        Ok(next)
    }

    pub fn files(&self) -> Vec<String> {
        [
            &self.base,
            &self.snapshots[0],
            &self.snapshots[1],
            &self.journal,
        ]
        .into_iter()
        .flat_map(TableRoot::files)
        .collect()
    }
}

pub(crate) fn base_entry(path: &SyncPath, entry: BaselineEntry) -> Entry {
    let mut value = encode(entry.left()).to_vec();
    value.extend(encode(entry.right()));
    Entry {
        key: path.as_bytes().to_vec(),
        value: Some(value),
    }
}

pub(crate) fn snapshot_entry(entry: &SnapshotEntry) -> Entry {
    let mut value = encode(entry.version).to_vec();
    value.extend(&entry.fingerprint);
    Entry {
        key: entry.path.as_bytes().to_vec(),
        value: Some(value),
    }
}

pub(crate) fn journal_entry(entry: &JournalEntry) -> io::Result<Entry> {
    let (action, version) = match entry.action {
        JournalAction::Create(v) => (1, v),
        JournalAction::Replace(v) => (2, v),
        JournalAction::Metadata(v) => (3, v),
        JournalAction::Archive(v) => (4, v),
        JournalAction::Move(v) => (5, v),
    };
    let mut value = vec![
        match entry.side {
            Side::Left => 0,
            Side::Right => 1,
        },
        action,
    ];
    value.extend(encode(version));
    value.push(u8::from(entry.before.is_some()));
    if let Some(base) = entry.before {
        value.extend(encode(base.left()));
        value.extend(encode(base.right()));
    }
    put(&mut value, entry.path.as_bytes(), TEXT_LIMIT)?;
    put(
        &mut value,
        entry.moved_from.as_ref().map_or(&[], SyncPath::as_bytes),
        TEXT_LIMIT,
    )?;
    Ok(Entry {
        key: entry.seq.to_be_bytes().to_vec(),
        value: Some(value),
    })
}

fn decode_journal(key: &[u8], value: &[u8]) -> io::Result<JournalEntry> {
    let seq = u64::from_be_bytes(
        key.try_into()
            .map_err(|_| invalid("Invalid target journal sequence"))?,
    );
    let mut input = Reader(value);
    let side = match input.take(1)?[0] {
        0 => Side::Left,
        1 => Side::Right,
        _ => return Err(invalid("Invalid target journal side")),
    };
    let action = input.take(1)?[0];
    let version = decode(input.take(VERSION_BYTES)?).map_err(other)?;
    let action = match action {
        1 => JournalAction::Create(version),
        2 => JournalAction::Replace(version),
        3 => JournalAction::Metadata(version),
        4 => JournalAction::Archive(version),
        5 => JournalAction::Move(version),
        _ => return Err(invalid("Invalid target journal action")),
    };
    let before = match input.take(1)?[0] {
        0 => None,
        1 => Some(
            decode_entry(input.take(VERSION_BYTES)?, input.take(VERSION_BYTES)?).map_err(other)?,
        ),
        _ => return Err(invalid("Invalid target journal baseline")),
    };
    let path = SyncPath::try_from(input.bytes(TEXT_LIMIT)?).map_err(other)?;
    let source = input.bytes(TEXT_LIMIT)?;
    let moved_from = if source.is_empty() {
        None
    } else {
        Some(SyncPath::try_from(source).map_err(other)?)
    };
    if matches!(action, JournalAction::Move(_)) != moved_from.is_some() {
        return Err(invalid("Invalid target journal move"));
    }
    input.end()?;
    Ok(JournalEntry {
        seq,
        path,
        side,
        action,
        before,
        moved_from,
    })
}

fn index(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}
pub(crate) fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn put(out: &mut Vec<u8>, bytes: &[u8], limit: usize) -> io::Result<()> {
    if bytes.len() > limit {
        return Err(invalid("Target catalog field exceeds limit"));
    }
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
    Ok(())
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if length > self.0.len() {
            return Err(invalid("Truncated target catalog"));
        }
        let (bytes, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("checked length"))
    }
    fn bytes(&mut self, limit: usize) -> io::Result<&'a [u8]> {
        let length = u32::from_le_bytes(self.array()?) as usize;
        if length > limit {
            return Err(invalid("Target catalog field exceeds limit"));
        }
        self.take(length)
    }
    fn text(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes(TEXT_LIMIT)?.to_vec()).map_err(other)
    }
    fn end(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("Trailing target catalog bytes"))
        }
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, digits) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        *byte = u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
    }
    Some(bytes)
}
