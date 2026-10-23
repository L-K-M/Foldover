use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_types::{LocalRoot, PairDraft, PairId, SyncMode};

use super::*;

struct Fixture {
    left: tempfile::TempDir,
    right: tempfile::TempDir,
    mode: SyncMode,
    revision: u64,
    base: BTreeMap<SyncPath, BaselineEntry>,
    identities: RecordedIdentities,
    resolutions: BTreeMap<SyncPath, foldover_types::Resolution>,
    skips: BTreeMap<SyncPath, foldover_types::Skip>,
    filters: Vec<String>,
    size_limit: Option<u64>,
    age_limit: Option<u32>,
    canary: Option<SyncPath>,
    durability: foldover_types::Durability,
    records: foldover_types::FolderRecords,
    bandwidth_limit: Option<u64>,
    right_kind: RightKind,
    journal: Option<foldover_state::Journal>,
    /// In-memory folders that replace both temporary directories.
    memory: Option<[foldover_sim::MemoryEndpoint; 2]>,
    leases: LeaseTiming,
}

/// How the fixture reaches its right folder.
#[derive(Clone, Copy, PartialEq)]
enum RightKind {
    Local,
    /// Through OpenSSH's `sftp-server` on standard streams.
    Sftp,
}

fn sftp_server() -> Option<std::path::PathBuf> {
    std::env::var_os("FOLDOVER_SFTP_SERVER")
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(
            [
                "/usr/libexec/sftp-server",
                "/usr/lib/openssh/sftp-server",
                "/usr/lib/ssh/sftp-server",
            ]
            .map(std::path::PathBuf::from),
        )
        .find(|path| path.exists())
}

fn fixture(mode: SyncMode) -> Fixture {
    Fixture {
        left: tempfile::tempdir().unwrap(),
        right: tempfile::tempdir().unwrap(),
        mode,
        revision: 1,
        base: BTreeMap::new(),
        identities: RecordedIdentities::default(),
        resolutions: BTreeMap::new(),
        skips: BTreeMap::new(),
        filters: Vec::new(),
        size_limit: None,
        age_limit: None,
        canary: None,
        durability: foldover_types::Durability::Standard,
        records: foldover_types::FolderRecords::Both,
        bandwidth_limit: None,
        right_kind: RightKind::Local,
        journal: None,
        memory: None,
        leases: TEST_LEASES,
    }
}

/// Applies commits like the state store, journal included.
struct Store<'a> {
    revision: u64,
    base: &'a mut BTreeMap<SyncPath, BaselineEntry>,
    identities: &'a mut RecordedIdentities,
    journal: &'a mut Option<foldover_state::Journal>,
}

impl Store<'_> {
    fn apply(&mut self, planned: u64, commit: Commit<'_>) -> Result<(), crate::Error> {
        if planned != self.revision {
            return Err(crate::Error::StaleRevision);
        }
        let updates = match commit {
            Commit::Identities { left, right } => {
                *self.identities = RecordedIdentities::new(Some(left), Some(right));
                return Ok(());
            }
            Commit::Base {
                run,
                updates,
                journal,
            } => {
                match self.journal {
                    _ if journal.is_empty() => {}
                    Some(current) if current.run == run => {
                        current.entries.extend_from_slice(journal)
                    }
                    _ => {
                        *self.journal = Some(foldover_state::Journal {
                            run: run.to_owned(),
                            entries: journal.to_vec(),
                        })
                    }
                }
                updates
            }
            Commit::Undoing { updates } => updates,
            Commit::Undid { run, seqs } => {
                if let Some(journal) = self.journal.as_mut().filter(|journal| journal.run == run) {
                    journal.entries.retain(|entry| !seqs.contains(&entry.seq));
                }
                return Ok(());
            }
            Commit::Undone { run, updates } => {
                if self
                    .journal
                    .as_ref()
                    .is_some_and(|journal| journal.run == run)
                {
                    *self.journal = None;
                }
                updates
            }
        };
        for update in updates {
            match update {
                BaseUpdate::Record(path, entry) => {
                    self.base.insert(path.clone(), *entry);
                }
                BaseUpdate::Forget(path) => {
                    self.base.remove(path);
                }
            }
        }
        Ok(())
    }
}

fn root(path: &Path) -> LocalRoot {
    LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
}

const STARTED: std::time::SystemTime = std::time::UNIX_EPOCH;

fn proceed() -> ScanControl {
    ScanControl::Continue
}

fn path(bytes: &[u8]) -> SyncPath {
    SyncPath::try_from(bytes).unwrap()
}

impl Fixture {
    fn prepare(&self) -> PairRun {
        self.try_prepare().unwrap()
    }

    fn inputs(&self) -> RunInputs {
        let filters = foldover_types::Filters::new(self.filters.clone())
            .and_then(|filters| filters.with_size_limit(self.size_limit))
            .and_then(|filters| filters.with_age_limit(self.age_limit))
            .unwrap();
        let draft = PairDraft::new(
            "pair".into(),
            root(self.left.path()),
            root(self.right.path()),
            self.mode,
        )
        .unwrap()
        .with_options(
            foldover_types::PairOptions::new(
                foldover_types::MetadataPolicy::default(),
                self.durability,
            )
            .with_filters(filters)
            .with_canary(self.canary.clone())
            .unwrap()
            .with_records(self.records)
            .with_bandwidth_limit(self.bandwidth_limit)
            .unwrap(),
        );
        let pair =
            PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), self.revision, draft)
                .unwrap();
        RunInputs::new(
            pair,
            self.base.clone(),
            self.identities,
            self.resolutions.clone(),
        )
        .with_skips(self.skips.clone())
        .with_lease_timing(self.leases)
    }

    fn try_prepare(&self) -> Result<PairRun, RunError> {
        let inputs = self.inputs();
        if let Some([left, right]) = &self.memory {
            let endpoints = foldover_endpoint::EndpointPair::new(
                foldover_endpoint::Endpoint::Memory(left.clone()),
                foldover_endpoint::Endpoint::Memory(right.clone()),
            );
            return PairRun::prepare_on(inputs, endpoints, &mut proceed, &NoProgress);
        }
        if self.right_kind == RightKind::Local {
            return PairRun::prepare(inputs, &mut proceed, &NoProgress);
        }
        let server = sftp_server().expect("callers check for sftp-server");
        let right = foldover_endpoint::SftpEndpoint::spawn(
            &server,
            &[std::ffi::OsStr::new("-d"), std::ffi::OsStr::new("/")],
            self.right.path().as_os_str().as_bytes(),
        )
        .unwrap();
        let left = foldover_endpoint::LocalEndpoint::open(self.left.path()).unwrap();
        let endpoints = foldover_endpoint::EndpointPair::new(
            foldover_endpoint::Endpoint::Local(left),
            foldover_endpoint::Endpoint::Sftp(right),
        );
        PairRun::prepare_on(inputs, endpoints, &mut proceed, &NoProgress)
    }

    /// Run with an in-memory base that applies commits like the state store.
    fn run(&mut self, approval: Approval, context: RunContext) -> RunOutcome {
        self.run_at(approval, context, STARTED)
    }

    fn run_at(
        &mut self,
        approval: Approval,
        context: RunContext,
        started: std::time::SystemTime,
    ) -> RunOutcome {
        let prepared = self.prepare();
        let mut store = self.store();
        prepared
            .execute(
                approval,
                context,
                started,
                &mut |planned, commit| store.apply(planned, commit),
                &mut proceed,
                &NoProgress,
            )
            .unwrap()
    }

    fn store(&mut self) -> Store<'_> {
        Store {
            revision: self.revision,
            base: &mut self.base,
            identities: &mut self.identities,
            journal: &mut self.journal,
        }
    }

    /// A first run stands for the reviewed preview a person accepted.
    fn sync(&mut self) -> RunReport {
        let context = if self.identities.side(Side::Left).is_none() {
            RunContext::Reviewed
        } else {
            RunContext::Interactive
        };
        match self.run(Approval::default(), context) {
            RunOutcome::Ran(report) => report,
            RunOutcome::NeedsApproval(preview) => {
                panic!("unexpected approval request: {preview:?}")
            }
        }
    }
}

/// PLAN.md §4.10: what iCloud keeps only in the cloud is skipped in both
/// folders, folders with their contents, rather than downloaded; it is
/// never taken for a deletion, and syncs again once it is back.
#[test]
fn entries_kept_only_in_icloud_are_skipped_never_deleted() {
    let mut pair = fixture(SyncMode::Sync);
    let folders = [
        foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks::default()),
        foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks::default()),
    ];
    pair.memory = Some(folders.clone());
    let [left, right] = &folders;
    for file in [b"notes.txt".as_slice(), b"photos/a.jpg", b"photos/b.jpg"] {
        left.put_file(file, b"v1").unwrap();
    }
    pair.sync();
    assert_eq!(right.user_files().len(), 3);

    // Evicted on the left, and edited on the right meanwhile.
    left.evict(b"notes.txt");
    left.evict(b"photos");
    right.put_file(b"photos/a.jpg", b"v2").unwrap();
    right.put_file(b"photos/c.jpg", b"new").unwrap();
    let report = pair.sync();
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!((report.copied(), report.archived()), (0, 0));
    assert_eq!(right.user_files().len(), 4);

    // Downloaded again, it syncs as usual.
    left.download(b"photos");
    let report = pair.sync();
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(report.copied(), 2);
    assert_eq!(left.user_files()[b"photos/a.jpg".as_slice()], b"v2");
    assert_eq!(left.user_files()[b"photos/c.jpg".as_slice()], b"new");
    assert_eq!(left.user_files()[b"notes.txt".as_slice()], b"v1");
}

#[test]
fn special_entries_protect_both_folders_and_the_base_while_other_files_sync() {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixListener;

    for mode in [
        SyncMode::Sync,
        SyncMode::Mirror { source: Side::Left },
        SyncMode::Mirror {
            source: Side::Right,
        },
    ] {
        for side in [Side::Left, Side::Right] {
            let mut pair = fixture(mode);
            let source = match mode {
                SyncMode::Mirror {
                    source: Side::Right,
                } => pair.right.path(),
                _ => pair.left.path(),
            };
            fs::create_dir(source.join("folder")).unwrap();
            for name in ["entry", "folder/child", "notes.txt"] {
                fs::write(source.join(name), b"v1").unwrap();
            }
            pair.sync();
            let base = pair.base.clone();
            let (special, other) = match side {
                Side::Left => (pair.left.path(), pair.right.path()),
                Side::Right => (pair.right.path(), pair.left.path()),
            };

            // A socket can replace a tracked file or folder; neither is a deletion.
            fs::remove_file(special.join("entry")).unwrap();
            fs::remove_dir_all(special.join("folder")).unwrap();
            let _file_socket = UnixListener::bind(special.join("entry")).unwrap();
            let _folder_socket = UnixListener::bind(special.join("folder")).unwrap();
            fs::write(other.join("entry"), b"edited").unwrap();
            fs::write(other.join("folder/child"), b"edited").unwrap();
            fs::write(other.join("folder/new"), b"new").unwrap();
            let source = match mode {
                SyncMode::Mirror {
                    source: Side::Right,
                } => pair.right.path(),
                _ => pair.left.path(),
            };
            fs::write(source.join("notes.txt"), b"v2").unwrap();

            let preview = pair.prepare().preview();
            assert_eq!(
                preview
                    .items()
                    .iter()
                    .map(|item| item.path().clone())
                    .collect::<Vec<_>>(),
                [path(b"notes.txt")]
            );
            let report = pair.sync();
            assert!(report.failures().is_empty(), "{:?}", report.failures());
            assert_eq!((report.copied(), report.archived()), (1, 0));
            for name in [b"entry".as_slice(), b"folder", b"folder/child"] {
                assert_eq!(pair.base[&path(name)], base[&path(name)]);
            }
            assert!(!pair.base.contains_key(&path(b"folder/new")));
            let (special, other) = match side {
                Side::Left => (pair.left.path(), pair.right.path()),
                Side::Right => (pair.right.path(), pair.left.path()),
            };
            for name in ["entry", "folder"] {
                assert!(
                    fs::symlink_metadata(special.join(name))
                        .unwrap()
                        .file_type()
                        .is_socket()
                );
            }
            assert_eq!(fs::read(other.join("entry")).unwrap(), b"edited");
            assert_eq!(fs::read(other.join("folder/child")).unwrap(), b"edited");
            assert_eq!(fs::read(other.join("folder/new")).unwrap(), b"new");
            assert_eq!(fs::read(other.join("notes.txt")).unwrap(), b"v2");

            // A regular file at the path participates again on the next run.
            fs::remove_file(special.join("entry")).unwrap();
            fs::write(special.join("entry"), b"v1").unwrap();
            assert_eq!(pair.prepare().preview().items()[0].path(), &path(b"entry"));
        }
    }
}

#[test]
fn unavailable_socket_paths_keep_the_baselines_unicode_spelling() {
    let mut pair = fixture(SyncMode::Sync);
    let composed = "caf\u{e9}";
    let decomposed = "cafe\u{301}";
    fs::create_dir(pair.left.path().join(composed)).unwrap();
    fs::write(pair.left.path().join(composed).join("child"), b"keep").unwrap();
    pair.sync();
    pair.base = pair
        .base
        .into_iter()
        .map(|(name, entry)| {
            let composed: String = std::str::from_utf8(name.as_bytes())
                .unwrap()
                .nfc()
                .collect();
            (path(composed.as_bytes()), entry)
        })
        .collect();
    fs::remove_dir_all(pair.left.path().join(composed)).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(pair.left.path().join(decomposed)).unwrap();
    let observe = |root| {
        foldover_endpoint::LocalEndpoint::open(root)
            .unwrap()
            .observe(foldover_types::MetadataPolicy::default(), &mut proceed)
            .unwrap()
    };
    let mut left = observe(pair.left.path());
    let right = observe(pair.right.path());
    let exact: BTreeSet<SyncPath> = unavailable(&left, &right, &pair.base, NameMatching::EXACT)
        .cloned()
        .collect();
    assert_eq!(exact, [path(decomposed.as_bytes())].into_iter().collect());
    let names = NameMatching::new(
        foldover_types::CaseMatching::Exact,
        UnicodeMatching::Normalized,
    );
    align_spellings(&mut left, &pair.base, names);
    assert_eq!(left.unavailable(), &[path(composed.as_bytes())]);
    let skipped: BTreeSet<SyncPath> = unavailable(&left, &right, &pair.base, names)
        .cloned()
        .collect();
    assert_eq!(
        skipped,
        [
            path(composed.as_bytes()),
            path(format!("{composed}/child").as_bytes())
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn unavailable_aliases_are_skipped_before_the_first_sync() {
    use foldover_types::CaseMatching;

    for (names, unavailable, counterpart) in [
        (
            NameMatching::new(CaseMatching::Exact, UnicodeMatching::Normalized),
            "cafe\u{301}",
            "caf\u{e9}",
        ),
        (
            NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Exact),
            "Socket",
            "socket",
        ),
    ] {
        let mut pair = fixture(SyncMode::Mirror { source: Side::Left });
        let folders = [
            foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks {
                names,
                ..foldover_sim::Quirks::default()
            }),
            foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks::default()),
        ];
        pair.memory = Some(folders.clone());
        let [left, right] = &folders;
        left.put_file(unavailable.as_bytes(), b"unreadable")
            .unwrap();
        left.evict(unavailable.as_bytes());
        right.put_file(counterpart.as_bytes(), b"keep").unwrap();
        let sibling = format!("{counterpart}-notes");
        left.put_file(sibling.as_bytes(), b"sync").unwrap();

        let preview = pair.prepare().preview();
        assert_eq!(preview.items().len(), 1);
        assert_eq!(preview.items()[0].path(), &path(sibling.as_bytes()));
        let report = pair.sync();
        assert!(report.failures().is_empty(), "{:?}", report.failures());
        assert_eq!((report.copied(), report.archived()), (1, 0));
        assert_eq!(right.user_files()[counterpart.as_bytes()], b"keep");
        assert_eq!(right.user_files()[sibling.as_bytes()], b"sync");
    }
}

#[test]
fn a_socket_canary_stops_before_writes_without_claiming_it_is_in_icloud() {
    let mut pair = fixture(SyncMode::Sync);
    let _socket = std::os::unix::net::UnixListener::bind(pair.left.path().join("canary")).unwrap();
    fs::write(pair.left.path().join("notes.txt"), b"keep").unwrap();
    pair.canary = Some(path(b"canary"));
    let error = pair
        .prepare()
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, _| panic!("an unavailable canary must prevent every write"),
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        RunError::CanaryUnavailable {
            side: Side::Left,
            ..
        }
    ));
    assert!(!error.to_string().contains("only in iCloud"));
    assert!(!pair.right.path().join("notes.txt").exists());
    assert!(!pair.left.path().join(".foldover").exists());
}

/// Review of dataless entries: an evicted canary stopped every run as
/// missing, telling a person to put back a file that was there.
#[test]
fn a_canary_kept_only_in_icloud_stops_the_run_and_says_so() {
    // Evicted alone, or with the folder that holds it.
    for (canary, evicted) in [
        (&b"canary.txt"[..], &b"canary.txt"[..]),
        (b"proof/canary.txt", b"proof"),
    ] {
        let mut pair = fixture(SyncMode::Sync);
        let folders = [
            foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks::default()),
            foldover_sim::MemoryEndpoint::new(foldover_sim::Quirks::default()),
        ];
        pair.memory = Some(folders.clone());
        let [left, _] = &folders;
        left.put_file(canary, b"here").unwrap();
        pair.sync();
        pair.canary = Some(path(canary));

        left.evict(evicted);
        let error = pair
            .prepare()
            .execute(
                Approval::default(),
                RunContext::Interactive,
                STARTED,
                &mut |_, _| panic!("nothing may commit"),
                &mut proceed,
                &NoProgress,
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                RunError::CanaryUnavailable {
                    side: Side::Left,
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn first_sync_copies_both_ways_and_a_second_run_has_nothing_to_do() {
    let mut pair = fixture(SyncMode::Sync);
    fs::create_dir(pair.left.path().join("photos")).unwrap();
    fs::write(pair.left.path().join("photos/a.jpg"), b"a").unwrap();
    fs::write(pair.right.path().join("notes.txt"), b"n").unwrap();

    let report = pair.sync();
    assert_eq!(report.copied(), 3);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(
        fs::read(pair.right.path().join("photos/a.jpg")).unwrap(),
        b"a"
    );
    assert_eq!(fs::read(pair.left.path().join("notes.txt")).unwrap(), b"n");
    assert_eq!(pair.base.len(), 3);

    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn edits_replace_and_archive_and_deletions_propagate_to_the_archive() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("edit"), b"v1").unwrap();
    fs::write(pair.left.path().join("delete"), b"d").unwrap();
    pair.sync();

    fs::write(pair.left.path().join("edit"), b"v2").unwrap();
    fs::remove_file(pair.right.path().join("delete")).unwrap();
    let preview = pair.prepare().preview();
    assert_eq!(
        preview
            .items()
            .iter()
            .map(|item| (item.path().clone(), item.action()))
            .collect::<Vec<_>>(),
        [
            (path(b"delete"), Action::Archive { on: Side::Left }),
            (
                path(b"edit"),
                Action::Copy {
                    to: Side::Right,
                    change: ChangeKind::ReplaceContent
                }
            ),
        ]
    );
    // Only copies move data; an archive moves nothing between folders.
    assert_eq!(
        preview
            .items()
            .iter()
            .map(PreviewItem::bytes)
            .collect::<Vec<_>>(),
        [0, 2]
    );

    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert_eq!(fs::read(pair.right.path().join("edit")).unwrap(), b"v2");
    assert_eq!(
        fs::read(
            pair.right
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/edit")
        )
        .unwrap(),
        b"v1"
    );
    assert!(!pair.left.path().join("delete").exists());
    assert_eq!(
        fs::read(
            pair.left
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/delete")
        )
        .unwrap(),
        b"d"
    );
    assert!(!pair.base.contains_key(&path(b"delete")));
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn conflicts_are_reported_and_both_versions_stay_untouched() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"base").unwrap();
    pair.sync();
    fs::write(pair.left.path().join("file"), b"left").unwrap();
    fs::write(pair.right.path().join("file"), b"right").unwrap();

    let report = pair.sync();
    assert_eq!(
        report.conflicts(),
        [(path(b"file"), ConflictKind::BothChanged)]
    );
    assert_eq!(fs::read(pair.left.path().join("file")).unwrap(), b"left");
    assert_eq!(fs::read(pair.right.path().join("file")).unwrap(), b"right");
}

#[test]
fn mass_deletions_need_approval_and_unattended_runs_stop() {
    let mut pair = fixture(SyncMode::Sync);
    // One file stays, so the emptied-folder gate does not decide this.
    for index in 0..=60 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    for index in 0..60 {
        fs::remove_file(pair.right.path().join(format!("{index}"))).unwrap();
    }

    let RunOutcome::NeedsApproval(preview) = pair.run(Approval::default(), RunContext::Interactive)
    else {
        panic!("expected an approval request");
    };
    assert_eq!(preview.deletions(Side::Left).deletions(), 60);
    // Every file plus the identity folder survives the refused run.
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 62);
    assert!(matches!(
        pair.run(Approval::new(60, 0), RunContext::Unattended),
        RunOutcome::NeedsApproval(_)
    ));
    assert!(matches!(
        pair.run(Approval::new(59, 0), RunContext::Interactive),
        RunOutcome::NeedsApproval(_)
    ));

    let RunOutcome::Ran(report) = pair.run(Approval::new(60, 0), RunContext::Interactive) else {
        panic!("approved deletions should run");
    };
    assert_eq!(report.archived(), 60);
}

#[test]
fn mirror_archives_target_extras_and_backup_keeps_them() {
    let mut mirror = fixture(SyncMode::Mirror { source: Side::Left });
    fs::write(mirror.left.path().join("kept"), b"k").unwrap();
    fs::write(mirror.right.path().join("extra"), b"e").unwrap();
    // A first run waits for a reviewed preview (§3.2, §3.6).
    assert!(matches!(
        mirror.run(Approval::default(), RunContext::Interactive),
        RunOutcome::NeedsApproval(_)
    ));
    assert!(mirror.right.path().join("extra").exists());
    let RunOutcome::Ran(report) = mirror.run(Approval::new(0, 1), RunContext::Reviewed) else {
        panic!("a reviewed approval runs the mirror")
    };
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert!(!mirror.right.path().join("extra").exists());
    assert!(
        mirror
            .right
            .path()
            .join(".foldover/archive/1970-01-01T00-00-00.000Z/extra")
            .exists()
    );

    let mut backup = fixture(SyncMode::Backup { source: Side::Left });
    fs::write(backup.left.path().join("kept"), b"k").unwrap();
    fs::write(backup.right.path().join("extra"), b"e").unwrap();
    let report = backup.sync();
    assert_eq!((report.copied(), report.archived()), (1, 0));
    assert!(backup.right.path().join("extra").exists());
}

#[test]
fn a_changed_pair_revision_stops_the_run() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"x").unwrap();
    let prepared = pair.prepare();
    pair.revision = 2;
    let result = prepared.execute(
        Approval::default(),
        RunContext::Reviewed,
        STARTED,
        &mut |planned, _| {
            if planned == pair.revision {
                Ok(())
            } else {
                Err(crate::Error::StaleRevision)
            }
        },
        &mut proceed,
        &NoProgress,
    );
    assert!(matches!(
        result,
        Err(RunError::Commit(crate::Error::StaleRevision))
    ));
}

#[test]
fn failed_folders_skip_their_contents_and_later_runs_recover_leftovers() {
    let mut pair = fixture(SyncMode::Mirror { source: Side::Left });
    fs::create_dir(pair.left.path().join("folder")).unwrap();
    fs::write(pair.left.path().join("folder/file"), b"x").unwrap();
    // A file where the folder must go makes its creation fail.
    let prepared = pair.prepare();
    fs::write(pair.right.path().join("folder"), b"blocker").unwrap();
    let mut base = BTreeMap::new();
    let RunOutcome::Ran(report) = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, commit| {
                if let Commit::Base { updates, .. } = commit {
                    for update in updates {
                        if let BaseUpdate::Record(path, entry) = update {
                            base.insert(path.clone(), *entry);
                        }
                    }
                }
                Ok(())
            },
            &mut proceed,
            &NoProgress,
        )
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(report.failures().len(), 1);
    assert_eq!(report.failures()[0].path(), &path(b"folder"));
    assert!(base.is_empty());
    assert_eq!(
        fs::read(pair.right.path().join("folder")).unwrap(),
        b"blocker"
    );

    fs::remove_file(pair.right.path().join("folder")).unwrap();
    fs::write(pair.right.path().join(".foldover-tmp-left"), b"displaced").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.recovered()), (2, 1));
    assert_eq!(
        fs::read(
            pair.right
                .path()
                .join(".foldover/archive/1970-01-01T00-00-00.000Z/.foldover-tmp-left")
        )
        .unwrap(),
        b"displaced"
    );
}

#[test]
fn a_replaced_folder_at_the_same_path_stops_before_any_change() {
    let mut pair = fixture(SyncMode::Sync);
    for index in 0..3 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    assert!(pair.identities.side(Side::Left).is_some());
    assert!(pair.identities.side(Side::Right).is_some());

    // An unmounted disk can leave an empty folder at the same path.
    let original = pair.right.path().with_extension("original");
    fs::rename(pair.right.path(), &original).unwrap();
    fs::create_dir(pair.right.path()).unwrap();
    assert!(matches!(
        pair.try_prepare(),
        Err(RunError::IdentityChanged { side: Side::Right })
    ));
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 4);

    fs::remove_dir(pair.right.path()).unwrap();
    fs::rename(&original, pair.right.path()).unwrap();
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn completed_runs_name_their_archive_and_prune_expired_runs() {
    let pair = fixture(SyncMode::Mirror { source: Side::Left });
    fs::write(pair.left.path().join("file"), b"x").unwrap();

    // A run 31 days after an archived one deletes it; a newer one stays.
    let day = std::time::Duration::from_secs(86_400);
    let archive = pair.right.path().join(".foldover/archive");
    for age in [31, 29] {
        let name = foldover_endpoint::archive_name(std::time::UNIX_EPOCH + day * (40 - age));
        fs::create_dir_all(archive.join(&name)).unwrap();
        fs::write(archive.join(name).join("kept"), b"k").unwrap();
    }
    let prepared = pair.prepare();
    let started = std::time::UNIX_EPOCH + day * 40;
    let RunOutcome::Ran(report) = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            started,
            &mut |_, _| Ok(()),
            &mut proceed,
            &NoProgress,
        )
        .unwrap()
    else {
        panic!("expected a run");
    };
    assert_eq!(report.archive(), foldover_endpoint::archive_name(started));
    assert_eq!(report.pruned(), 1);
    assert!(
        !archive
            .join(foldover_endpoint::archive_name(
                std::time::UNIX_EPOCH + day * 9
            ))
            .exists()
    );
    assert!(
        archive
            .join(foldover_endpoint::archive_name(
                std::time::UNIX_EPOCH + day * 11
            ))
            .exists()
    );
}

#[test]
fn resolved_conflicts_keep_the_chosen_side_until_either_side_changes() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"base").unwrap();
    fs::write(pair.left.path().join("gone"), b"base").unwrap();
    pair.sync();
    fs::write(pair.left.path().join("file"), b"left").unwrap();
    fs::write(pair.right.path().join("file"), b"right").unwrap();
    fs::remove_file(pair.left.path().join("gone")).unwrap();
    fs::write(pair.right.path().join("gone"), b"edited").unwrap();

    let preview = pair.prepare().preview();
    let versions = |name: &[u8]| {
        preview
            .items()
            .iter()
            .find(|item| item.path() == &path(name))
            .unwrap()
            .versions()
    };
    let (left, right) = versions(b"file");
    pair.resolutions.insert(
        path(b"file"),
        foldover_types::Resolution::new(Side::Right, left, right),
    );
    let (left, right) = versions(b"gone");
    pair.resolutions.insert(
        path(b"gone"),
        foldover_types::Resolution::new(Side::Left, left, right),
    );

    let report = pair.sync();
    assert!(report.conflicts().is_empty(), "{:?}", report.conflicts());
    assert_eq!((report.copied(), report.archived()), (1, 1));
    assert_eq!(fs::read(pair.left.path().join("file")).unwrap(), b"right");
    let archive = pair
        .left
        .path()
        .join(".foldover/archive/1970-01-01T00-00-00.000Z");
    assert_eq!(fs::read(archive.join("file")).unwrap(), b"left");
    assert!(!pair.right.path().join("gone").exists());

    // A choice made for other versions no longer applies.
    fs::write(pair.left.path().join("file"), b"left again").unwrap();
    fs::write(pair.right.path().join("file"), b"right again").unwrap();
    let report = pair.sync();
    assert_eq!(
        report.conflicts(),
        [(path(b"file"), ConflictKind::BothChanged)]
    );
}

/// A folder deleted on one side while a file inside it changed on the
/// other is one conflict at the folder; choosing a side settles the folder
/// and everything in it.
#[test]
fn a_folder_choice_settles_the_whole_folder() {
    for keep in [Side::Left, Side::Right] {
        let mut pair = fixture(SyncMode::Sync);
        fs::create_dir(pair.left.path().join("photos")).unwrap();
        fs::write(pair.left.path().join("photos/a.jpg"), b"a").unwrap();
        fs::write(pair.left.path().join("photos/b.jpg"), b"b").unwrap();
        pair.sync();
        fs::write(pair.left.path().join("photos/a.jpg"), b"edited").unwrap();
        fs::remove_dir_all(pair.right.path().join("photos")).unwrap();

        let preview = pair.prepare().preview();
        let folder = preview
            .items()
            .iter()
            .find(|item| item.path() == &path(b"photos"))
            .unwrap();
        assert_eq!(
            folder.action(),
            Action::Conflict(ConflictKind::DeletedVersusEdited)
        );
        let (left, right) = folder.versions();
        pair.resolutions.insert(
            path(b"photos"),
            foldover_types::Resolution::new(keep, left, right),
        );

        // Keeping B's deletion empties folder A, so the choice runs as the
        // reviewed preview a person accepts after choosing (§3.6).
        let RunOutcome::Ran(report) = pair.run(Approval::new(3, 0), RunContext::Reviewed) else {
            panic!("a reviewed choice runs");
        };
        assert!(report.conflicts().is_empty(), "{:?}", report.conflicts());
        assert!(report.failures().is_empty(), "{:?}", report.failures());
        match keep {
            Side::Left => {
                let photos = pair.right.path().join("photos");
                assert_eq!(fs::read(photos.join("a.jpg")).unwrap(), b"edited");
                assert_eq!(fs::read(photos.join("b.jpg")).unwrap(), b"b");
            }
            Side::Right => {
                assert!(!pair.left.path().join("photos").exists());
                let archive = pair
                    .left
                    .path()
                    .join(".foldover/archive/1970-01-01T00-00-00.000Z/photos");
                assert_eq!(fs::read(archive.join("a.jpg")).unwrap(), b"edited");
                assert_eq!(fs::read(archive.join("b.jpg")).unwrap(), b"b");
            }
        }
        assert!(pair.prepare().preview().items().is_empty());
    }
}

#[test]
fn earlier_snapshots_supply_versions_for_settled_unchanged_entries() {
    let pair = fixture(SyncMode::Sync);
    let file = pair.left.path().join("file");
    fs::write(&file, b"data").unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let first = pair.prepare();

    // A deliberately wrong stored version shows whether it was reused.
    let stored = foldover_types::EntryVersion::new(
        foldover_types::EntryContent::File(foldover_types::Digest::from_bytes([7; 32])),
        foldover_types::Digest::from_bytes([7; 32]),
    );
    let left: BTreeMap<_, _> = first
        .snapshot(Side::Left)
        .into_iter()
        .map(|entry| {
            let observed =
                foldover_endpoint::ObservedEntry::restore(stored, &entry.fingerprint).unwrap();
            (entry.path, observed)
        })
        .collect();
    let draft = PairDraft::new(
        "pair".into(),
        root(pair.left.path()),
        root(pair.right.path()),
        pair.mode,
    )
    .unwrap();
    let config =
        PairConfig::from_stored(PairId::from_bytes([1; 16]).unwrap(), pair.revision, draft)
            .unwrap();
    let inputs = RunInputs::new(config, BTreeMap::new(), pair.identities, BTreeMap::new())
        .with_snapshots(left, BTreeMap::new());
    let second = PairRun::prepare(inputs, &mut proceed, &NoProgress).unwrap();
    let preview = second.preview();
    assert_eq!(preview.items()[0].versions().0, Some(stored));
}

#[test]
fn filters_from_the_pair_and_both_ignore_files_exclude_entries_on_both_sides() {
    let mut pair = fixture(SyncMode::Sync);
    pair.filters = vec!["*.tmp".into()];
    fs::write(pair.left.path().join("a.tmp"), b"tmp").unwrap();
    fs::write(pair.left.path().join("keep"), b"k").unwrap();
    fs::create_dir(pair.right.path().join("cache")).unwrap();
    fs::write(pair.right.path().join("cache/blob"), b"b").unwrap();
    // Only the right root names the cache; both sides honor it.
    fs::write(pair.right.path().join(".foldoverignore"), b"cache/\n").unwrap();
    pair.sync();
    assert!(!pair.right.path().join("a.tmp").exists());
    assert!(pair.right.path().join("keep").exists());
    assert!(!pair.left.path().join("cache").exists());
    assert!(pair.left.path().join(".foldoverignore").exists());

    // Excluding an already synced file forgets it instead of deleting it.
    fs::write(pair.left.path().join(".foldoverignore"), b"cache/\nkeep\n").unwrap();
    pair.sync();
    fs::remove_file(pair.left.path().join("keep")).unwrap();
    let report = pair.sync();
    assert_eq!(report.archived(), 0);
    assert!(pair.right.path().join("keep").exists());
}

#[test]
fn runs_count_the_bytes_each_side_receives_and_check_free_space() {
    let pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("big"), vec![1; 4096]).unwrap();
    fs::write(pair.right.path().join("small"), vec![2; 100]).unwrap();
    fs::create_dir(pair.right.path().join("folder")).unwrap();
    let run = pair.prepare();
    assert_eq!(run.bytes_to_copy(Side::Right), 4096);
    assert_eq!(run.bytes_to_copy(Side::Left), 100);
    // A temporary volume has room for this.
    assert!(
        pair.prepare()
            .endpoints
            .endpoint(Side::Left)
            .available_space()
            .unwrap()
            > 0
    );
}

#[test]
fn copies_need_their_size_plus_the_reserve() {
    let reserve = super::SPACE_RESERVE;
    assert!(super::require_space(Side::Left, 10, 10 + reserve).is_ok());
    assert!(matches!(
        super::require_space(Side::Right, 11, 10 + reserve),
        Err(RunError::InsufficientSpace {
            side: Side::Right,
            needed: 11,
            ..
        })
    ));
    assert!(super::require_space(Side::Left, u64::MAX, u64::MAX).is_err());
}

#[test]
fn files_too_large_in_either_folder_are_skipped_in_both() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("small"), b"a").unwrap();
    fs::write(pair.left.path().join("big"), b"0123456789").unwrap();
    pair.sync();

    pair.size_limit = Some(5);
    assert!(pair.prepare().preview().items().is_empty());

    // Shrinking it on one side changes nothing while the other is too large.
    fs::write(pair.left.path().join("big"), b"xyz").unwrap();
    fs::write(pair.left.path().join("small"), b"b").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (1, 0));
    assert_eq!(
        fs::read(pair.right.path().join("big")).unwrap(),
        b"0123456789"
    );
    assert_eq!(fs::read(pair.left.path().join("big")).unwrap(), b"xyz");
    assert_eq!(fs::read(pair.right.path().join("small")).unwrap(), b"b");

    // The base row survived the skip, so the edit made meanwhile syncs.
    pair.size_limit = None;
    let preview = pair.prepare().preview();
    assert_eq!(
        preview
            .items()
            .iter()
            .map(|item| (item.path().clone(), item.action()))
            .collect::<Vec<_>>(),
        [(
            path(b"big"),
            Action::Copy {
                to: Side::Right,
                change: ChangeKind::ReplaceContent
            }
        )]
    );
}

#[test]
fn files_old_in_both_folders_are_skipped_until_edited() {
    let mut pair = fixture(SyncMode::Sync);
    let age = |name: &str, folder: &Path| {
        let file = fs::File::options()
            .write(true)
            .open(folder.join(name))
            .unwrap();
        let hundred_days = std::time::Duration::from_secs(100 * 24 * 60 * 60);
        file.set_modified(std::time::SystemTime::now() - hundred_days)
            .unwrap();
    };
    // Synced long ago: the copy carries the old modification time.
    fs::write(pair.left.path().join("kept"), b"k").unwrap();
    age("kept", pair.left.path());
    pair.sync();
    fs::write(pair.left.path().join("stale"), b"s").unwrap();
    age("stale", pair.left.path());
    fs::write(pair.left.path().join("fresh"), b"f").unwrap();

    pair.age_limit = Some(30);
    let report = pair.sync();
    assert_eq!(report.copied(), 1);
    assert!(pair.right.path().join("fresh").exists());
    assert!(!pair.right.path().join("stale").exists());

    // An edit makes it recent on one side: it syncs as an edit.
    fs::write(pair.left.path().join("kept"), b"edited").unwrap();
    let report = pair.sync();
    assert_eq!((report.copied(), report.conflicts().len()), (1, 0));
    assert_eq!(fs::read(pair.right.path().join("kept")).unwrap(), b"edited");
}

#[test]
fn age_excluded_files_stay_in_the_snapshot_cache_for_reuse() {
    let mut pair = fixture(SyncMode::Sync);
    let file = pair.left.path().join("old");
    fs::write(&file, b"data").unwrap();
    let hundred_days = std::time::Duration::from_secs(100 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - hundred_days)
        .unwrap();
    pair.age_limit = Some(30);

    // Old on the only side that has it: skipped in both, so not planned.
    let first = pair.prepare();
    assert!(first.preview().items().is_empty());
    // The observation still feeds the next run's cache.
    let cached = first
        .snapshot(Side::Left)
        .into_iter()
        .find(|entry| entry.path == path(b"old"))
        .expect("an age-excluded file stays cached");

    // A wrong stored version proves the next run reused it without rereading.
    let stored = foldover_types::EntryVersion::new(
        foldover_types::EntryContent::File(foldover_types::Digest::from_bytes([7; 32])),
        foldover_types::Digest::from_bytes([7; 32]),
    );
    let left: BTreeMap<_, _> = [(
        cached.path.clone(),
        foldover_endpoint::ObservedEntry::restore(stored, &cached.fingerprint).unwrap(),
    )]
    .into_iter()
    .collect();
    let inputs = pair.inputs().with_snapshots(left, BTreeMap::new());
    let second = PairRun::prepare(inputs, &mut proceed, &NoProgress).unwrap();
    assert!(second.preview().items().is_empty());
    assert_eq!(
        second
            .snapshot(Side::Left)
            .iter()
            .find(|entry| entry.path == path(b"old"))
            .map(|entry| entry.version),
        Some(stored)
    );
}

const MIB: usize = 1024 * 1024;

/// Content big enough to resume.
fn resumable_content() -> Vec<u8> {
    (0..RESUMABLE_BYTES)
        .map(|index| (index % 253) as u8)
        .collect()
}

const RESUMABLE_BYTES: usize = 40 * MIB;
/// The least a broken-off copy keeps (the driver's `PARTIAL_MIN_BYTES`).
const PARTIAL_MIN_BYTES: u64 = 16 * MIB as u64;
/// Paces `resumable_content` to 2.5 s. Copies on workers ask the control
/// only every 100 ms, not once per read as on the run's thread; paced,
/// a copy moves 1.6 MiB between two asks, never the whole rest.
const RESUMABLE_PACE: u64 = 16 * MIB as u64;
/// Where a test breaks the copy off: past the 16 MiB a kept partial
/// needs, well before the end.
const BREAK_OFF_AT: u64 = 20 * MIB as u64;

/// Bytes a copy into `folder` staged so far.
fn staged_bytes(folder: &Path) -> u64 {
    fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".foldover-tmp-")
        })
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Run a paced copy into the left folder, cancel it once `BREAK_OFF_AT`
/// arrived, then lift the pace; returns the partial it kept.
fn break_off(pair: &mut Fixture) -> std::path::PathBuf {
    pair.bandwidth_limit = Some(RESUMABLE_PACE);
    let prepared = pair.prepare();
    pair.bandwidth_limit = None;
    let left = pair.left.path().to_owned();
    let mut store = pair.store();
    let outcome = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            at(1),
            &mut |planned, commit| store.apply(planned, commit),
            &mut || match staged_bytes(&left) >= BREAK_OFF_AT {
                true => ScanControl::Cancel,
                false => ScanControl::Continue,
            },
            &NoProgress,
        )
        .unwrap();
    let RunOutcome::Ran(report) = outcome else {
        panic!("unexpected approval request");
    };
    assert_eq!(report.copied(), 0);
    let mut kept = fs::read_dir(pair.left.path().join(".foldover/partial"))
        .unwrap()
        .map(|entry| entry.unwrap().path());
    let partial = kept.next().expect("a kept partial");
    assert!(kept.next().is_none());
    partial
}

/// PLAN.md §4.9: copies to and from a server run on several sessions at
/// once, each file whole and recorded once.
#[test]
fn many_files_reach_a_server_and_come_back_on_several_sessions() {
    const FOLDERS: usize = 3;
    const FILES: usize = 20;
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let file = |folder: usize, file: usize| format!("folder {folder}/file {file}.txt");
    for folder in 0..FOLDERS {
        fs::create_dir(left.join(format!("folder {folder}"))).unwrap();
        for index in 0..FILES {
            fs::write(
                left.join(file(folder, index)),
                file(folder, index).repeat(500),
            )
            .unwrap();
        }
    }

    let report = pair.sync();
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(report.copied(), (FOLDERS + FOLDERS * FILES) as u64);
    for folder in 0..FOLDERS {
        for index in 0..FILES {
            let path = file(folder, index);
            assert_eq!(
                fs::read(right.join(&path)).unwrap(),
                fs::read(left.join(&path)).unwrap()
            );
        }
    }
    assert!(pair.prepare().preview().items().is_empty());

    // Back from the server: it is the source now.
    for index in 0..FILES {
        fs::write(right.join(file(0, index)), format!("edited {index}")).unwrap();
    }
    let report = pair.sync();
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!(report.copied(), FILES as u64);
    for index in 0..FILES {
        assert_eq!(
            fs::read_to_string(left.join(file(0, index))).unwrap(),
            format!("edited {index}")
        );
    }
    assert!(pair.prepare().preview().items().is_empty());
}

/// PLAN.md §4.9: a pair's bandwidth limit paces its copies, local and
/// streamed alike, across all of a run's files.
#[test]
fn a_bandwidth_limit_paces_copies() {
    const LIMIT: u64 = 1_000_000;
    const FILE_BYTES: usize = 2_000_000;
    // Two files' data at the limit takes 4 s, well beyond a run's own
    // cost; allow for clock grain.
    const AT_LEAST: Duration = Duration::from_millis(3_950);
    for kind in [RightKind::Local, RightKind::Sftp] {
        if kind == RightKind::Sftp && sftp_server().is_none() {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            continue;
        }
        let mut pair = fixture(SyncMode::Sync);
        pair.right_kind = kind;
        pair.bandwidth_limit = Some(LIMIT);
        for name in ["a.bin", "b.bin"] {
            fs::write(pair.left.path().join(name), vec![7; FILE_BYTES]).unwrap();
        }

        let started = std::time::Instant::now();
        let report = pair.sync();
        assert_eq!(report.copied(), 2, "{:?}", report.failures());
        assert!(started.elapsed() >= AT_LEAST, "{:?}", started.elapsed());
        assert_eq!(
            fs::read(pair.right.path().join("b.bin")).unwrap().len(),
            FILE_BYTES
        );
    }
}

/// PLAN.md §4.9: a large copy that breaks off keeps what arrived, and the
/// next run fetches only the rest.
#[test]
fn a_broken_off_copy_from_a_server_resumes_where_it_stopped() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let data = resumable_content();
    fs::write(right.join("big.bin"), &data).unwrap();

    let partial = break_off(&mut pair);
    let kept = fs::metadata(&partial).unwrap().len();
    assert!(
        (PARTIAL_MIN_BYTES..RESUMABLE_BYTES as u64).contains(&kept),
        "{kept}"
    );
    assert!(!left.join("big.bin").exists());

    let report = pair.sync();
    assert_eq!(report.copied(), 1, "{:?}", report.failures());
    assert!(fs::read(left.join("big.bin")).unwrap() == data);
    assert!(!partial.exists());
}

/// PLAN.md §4.10: a decomposed name reaches an SFTP server composed; the
/// next run finds nothing to do, and an edit there comes back to the
/// spelling the pair knows.
#[test]
fn decomposed_names_reach_a_server_composed_and_stay_in_sync() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let (decomposed, composed) = ("cafe\u{301}", "caf\u{e9}");
    fs::create_dir(left.join(decomposed)).unwrap();
    fs::write(left.join(decomposed).join("menu.txt"), b"soup").unwrap();

    let report = pair.sync();
    assert_eq!(report.copied(), 2, "{:?}", report.failures());
    let names = |folder: &Path| -> Vec<std::ffi::OsString> {
        fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != ".foldover")
            .collect()
    };
    assert_eq!(names(&right), [composed]);
    assert!(pair.prepare().preview().items().is_empty());

    fs::write(right.join(composed).join("menu.txt"), b"soup and bread").unwrap();
    let report = pair.sync();
    assert_eq!(report.copied(), 1, "{:?}", report.failures());
    assert_eq!(names(&left), [decomposed]);
    assert_eq!(
        fs::read(left.join(decomposed).join("menu.txt")).unwrap(),
        b"soup and bread"
    );
    assert_eq!(names(&right), [composed]);
    assert!(pair.prepare().preview().items().is_empty());
}

/// A kept part that no longer matches fails its copy once, then the copy
/// starts over.
#[test]
fn a_corrupt_partial_is_dropped_and_the_copy_starts_over() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let data = resumable_content();
    fs::write(right.join("big.bin"), &data).unwrap();
    let partial = break_off(&mut pair);
    let mut garbled = fs::read(&partial).unwrap();
    garbled[MIB] ^= 0xff;
    fs::write(&partial, garbled).unwrap();

    let report = pair.sync();
    assert_eq!(report.copied(), 0);
    assert_eq!(report.failures().len(), 1, "{:?}", report.failures());
    assert!(!partial.exists());
    assert!(!left.join("big.bin").exists());

    let report = pair.sync();
    assert_eq!(report.copied(), 1, "{:?}", report.failures());
    assert!(fs::read(left.join("big.bin")).unwrap() == data);
}

/// Review of #83: a large copy that arrived whole but could not be
/// placed, because the file it replaces changed meanwhile, threw its data
/// away; the next attempt fetched it all again.
#[test]
fn a_whole_copy_that_cannot_be_placed_keeps_its_data() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("big.bin"), b"old").unwrap();
    pair.sync();
    let data = resumable_content();
    fs::write(right.join("big.bin"), &data).unwrap();

    // Partway through the paced copy, someone edits the file it replaces.
    pair.bandwidth_limit = Some(RESUMABLE_PACE);
    let prepared = pair.prepare();
    let mut store = pair.store();
    let mut edited = false;
    let outcome = prepared
        .execute(
            Approval::default(),
            RunContext::Interactive,
            at(1),
            &mut |planned, commit| store.apply(planned, commit),
            &mut || {
                if !edited && staged_bytes(&left) >= BREAK_OFF_AT {
                    fs::write(left.join("big.bin"), b"edited here").unwrap();
                    edited = true;
                }
                ScanControl::Continue
            },
            &NoProgress,
        )
        .unwrap();
    let RunOutcome::Ran(report) = outcome else {
        panic!("unexpected approval request");
    };
    assert!(edited, "the copy ended before the edit");
    assert_eq!(report.copied(), 0);
    assert_eq!(report.failures().len(), 1, "{:?}", report.failures());
    assert_eq!(fs::read(left.join("big.bin")).unwrap(), b"edited here");
    let kept: Vec<u64> = fs::read_dir(left.join(".foldover/partial"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(kept, [data.len() as u64]);
}

#[test]
fn sftp_folders_sync_like_local_ones() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    fs::create_dir(pair.left.path().join("docs")).unwrap();
    fs::write(pair.left.path().join("docs/a.txt"), b"a").unwrap();
    fs::write(pair.left.path().join("top"), vec![3; 300_000]).unwrap();
    std::os::unix::fs::symlink("top", pair.left.path().join("link")).unwrap();
    let report = pair.sync();
    assert_eq!(report.copied(), 4, "{:?}", report.failures());
    assert_eq!(
        fs::read(pair.right.path().join("docs/a.txt")).unwrap(),
        b"a"
    );
    assert_eq!(
        fs::read(pair.right.path().join("top")).unwrap(),
        vec![3; 300_000]
    );
    assert!(pair.right.path().join(".foldover/id").exists());
    assert!(pair.prepare().preview().items().is_empty());

    // An edit on the server comes back; a deletion here is archived there.
    fs::write(
        pair.right.path().join("docs/a.txt"),
        b"edited on the server",
    )
    .unwrap();
    fs::remove_file(pair.left.path().join("top")).unwrap();
    let report = pair.sync();
    assert_eq!(
        (report.copied(), report.archived()),
        (1, 1),
        "{:?}",
        report.failures()
    );
    assert_eq!(
        fs::read(pair.left.path().join("docs/a.txt")).unwrap(),
        b"edited on the server"
    );
    assert!(!pair.right.path().join("top").exists());
    let archived = pair
        .right
        .path()
        .join(".foldover/archive")
        .join(report.archive())
        .join("top");
    assert_eq!(fs::read(archived).unwrap(), vec![3; 300_000]);

    // Edits on both sides are a conflict; neither is overwritten.
    fs::write(pair.left.path().join("docs/a.txt"), b"here").unwrap();
    fs::write(pair.right.path().join("docs/a.txt"), b"there").unwrap();
    let report = pair.sync();
    assert_eq!(report.conflicts().len(), 1);
    assert_eq!(
        fs::read(pair.right.path().join("docs/a.txt")).unwrap(),
        b"there"
    );
}

/// Full durability cannot be promised to an SFTP folder; the run must
/// refuse before anything mutates either side, identity files included.
#[test]
fn full_durability_rejects_a_run_with_an_sftp_side() {
    if sftp_server().is_none() {
        eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
        return;
    }
    let mut pair = fixture(SyncMode::Sync);
    pair.right_kind = RightKind::Sftp;
    pair.durability = foldover_types::Durability::Full;
    fs::write(pair.left.path().join("report"), b"new").unwrap();
    let error = pair
        .prepare()
        .execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, _| panic!("nothing may commit"),
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::UnsupportedDurability { side: Side::Right }),
        "{error:?}"
    );
    // Nothing was written: no identities, no copy, no archive.
    assert!(!pair.left.path().join(".foldover").exists());
    assert!(!pair.right.path().join(".foldover").exists());
    assert!(!pair.right.path().join("report").exists());
    assert_eq!(fs::read(pair.left.path().join("report")).unwrap(), b"new");
}

#[test]
fn a_first_run_waits_for_a_reviewed_preview_even_when_it_only_copies() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("new"), b"n").unwrap();

    for context in [RunContext::Interactive, RunContext::Unattended] {
        let RunOutcome::NeedsApproval(preview) = pair.run(Approval::default(), context) else {
            panic!("an unreviewed first run must wait");
        };
        assert_eq!(preview.items().len(), 1);
    }
    // Nothing was written: no identities, no copy.
    assert!(pair.identities.side(Side::Left).is_none());
    assert!(!pair.left.path().join(".foldover").exists());
    assert_eq!(fs::read_dir(pair.right.path()).unwrap().count(), 0);

    let RunOutcome::Ran(report) = pair.run(Approval::default(), RunContext::Reviewed) else {
        panic!("a reviewed first run runs");
    };
    assert_eq!(report.copied(), 1);
    assert!(pair.identities.side(Side::Right).is_some());
}

#[test]
fn a_suddenly_empty_folder_needs_review_before_the_other_side_is_emptied() {
    let mut pair = fixture(SyncMode::Sync);
    for index in 0..3 {
        fs::write(pair.left.path().join(format!("{index}")), b"x").unwrap();
    }
    pair.sync();
    for index in 0..3 {
        fs::remove_file(pair.right.path().join(format!("{index}"))).unwrap();
    }

    // Three deletions are far below the mass-deletion fuse.
    for context in [RunContext::Interactive, RunContext::Unattended] {
        assert!(matches!(
            pair.run(Approval::new(3, 0), context),
            RunOutcome::NeedsApproval(_)
        ));
    }
    assert_eq!(fs::read_dir(pair.left.path()).unwrap().count(), 4);
    let RunOutcome::Ran(report) = pair.run(Approval::new(3, 0), RunContext::Reviewed) else {
        panic!("a reviewed plan runs");
    };
    assert_eq!(report.archived(), 3);

    // An emptied Backup target only receives copies again: no review.
    let mut backup = fixture(SyncMode::Backup { source: Side::Left });
    fs::write(backup.left.path().join("kept"), b"k").unwrap();
    backup.sync();
    fs::remove_file(backup.right.path().join("kept")).unwrap();
    assert_eq!(backup.sync().copied(), 1);
}

/// PLAN.md §3.6: the canary proves the right folders are mounted; a
/// missing or changed one stops the run before anything writes.
#[test]
fn a_missing_or_changed_canary_stops_the_run_before_any_write() {
    let mut pair = fixture(SyncMode::Sync);
    pair.canary = Some(path(b"docs/canary.txt"));
    fs::create_dir(pair.left.path().join("docs")).unwrap();
    fs::write(pair.left.path().join("docs/canary.txt"), b"canary").unwrap();
    fs::write(pair.left.path().join("file"), b"x").unwrap();

    // Missing on the right: nothing is written, even fully reviewed.
    let error = pair
        .prepare()
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, _| panic!("nothing may commit"),
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            RunError::CanaryMissing {
                side: Side::Right,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(pair.right.path()).unwrap().count(), 0);
    assert!(!pair.left.path().join(".foldover").exists());

    // Kept unchanged in both folders, it lets the run proceed. The right's
    // copy carries the left's mtime, like a preserved copy would, so the
    // first sync records the canary in the base.
    fs::create_dir(pair.right.path().join("docs")).unwrap();
    fs::write(pair.right.path().join("docs/canary.txt"), b"canary").unwrap();
    let modified = fs::metadata(pair.left.path().join("docs/canary.txt"))
        .unwrap()
        .modified()
        .unwrap();
    fs::File::open(pair.right.path().join("docs/canary.txt"))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    pair.sync();
    assert_eq!(fs::read(pair.right.path().join("file")).unwrap(), b"x");

    // Changed on a side since the base: CanaryChanged, again no writes.
    fs::write(pair.right.path().join("docs/canary.txt"), b"changed").unwrap();
    fs::write(pair.left.path().join("new"), b"n").unwrap();
    let error = pair
        .prepare()
        .execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, _| panic!("nothing may commit"),
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            RunError::CanaryChanged {
                side: Side::Right,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!pair.right.path().join("new").exists());
    assert_eq!(
        fs::read(pair.right.path().join("docs/canary.txt")).unwrap(),
        b"changed"
    );
}

#[test]
fn a_root_whose_identity_changes_mid_run_stops_at_the_next_batch() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("first"), b"f").unwrap();
    pair.sync();
    for index in 0..300 {
        fs::write(pair.left.path().join(format!("{index:03}")), b"x").unwrap();
    }

    let prepared = pair.prepare();
    let identity = pair.right.path().join(".foldover/id");
    let other = EndpointId::from_bytes([7; 16]);
    let mut calls = 0;
    let mut control = || {
        calls += 1;
        if calls == 10 {
            fs::write(&identity, format!("{other}\n")).unwrap();
        }
        ScanControl::Continue
    };
    let base = &mut pair.base;
    let error = prepared
        .execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, commit| {
                if let Commit::Base { updates, .. } = commit {
                    for update in updates {
                        if let BaseUpdate::Record(path, entry) = update {
                            base.insert(path.clone(), *entry);
                        }
                    }
                }
                Ok(())
            },
            &mut control,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::IdentityChanged { side: Side::Right }),
        "{error:?}"
    );

    // Writes up to the check landed and were recorded; the rest waited.
    let copied = fs::read_dir(pair.right.path()).unwrap().count() - 1;
    assert_eq!(copied, pair.base.len());
    assert_eq!(copied, 1 + COMMIT_BATCH);
}

const HOUR: std::time::Duration = std::time::Duration::from_secs(3_600);

/// Leases that give up on a busy folder at once and count a still one as
/// dead quickly.
const TEST_LEASES: LeaseTiming = LeaseTiming {
    ttl: std::time::Duration::from_millis(200),
    beat: std::time::Duration::from_millis(50),
    poll: std::time::Duration::from_millis(20),
    wait: std::time::Duration::ZERO,
};

/// PLAN.md §4: while another Mac's run holds a folder's lease, a run
/// writes nothing and says who is syncing.
#[test]
fn a_folder_another_mac_is_syncing_waits_until_its_lease_is_free_or_dead() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("new.txt"), b"n").unwrap();
    let lock = right.join(".foldover/lock");
    fs::write(
        &lock,
        "foldover-lease 1\nholder other/run\nlabel Studio\nbeat 1\n",
    )
    .unwrap();

    let prepared = pair.prepare();
    let mut store = pair.store();
    let busy = prepared.execute(
        Approval::default(),
        RunContext::Interactive,
        at(1),
        &mut |planned, commit| store.apply(planned, commit),
        &mut proceed,
        &NoProgress,
    );
    assert!(
        matches!(&busy, Err(RunError::Lease(LeaseError::Busy { label })) if label == "Studio"),
        "{busy:?}"
    );
    assert!(!right.join("new.txt").exists());
    // Released: the run proceeds and leaves no lease behind.
    fs::remove_file(&lock).unwrap();
    pair.ran(Approval::default(), at(2));
    assert_eq!(fs::read(right.join("new.txt")).unwrap(), b"n");
    assert!(!lock.exists());
}

/// Runs `hours` after the first, so each gets its own archive folder.
fn at(hours: u32) -> std::time::SystemTime {
    STARTED + HOUR * hours
}

impl Fixture {
    fn ran(&mut self, approval: Approval, started: std::time::SystemTime) -> RunReport {
        match self.run_at(approval, RunContext::Interactive, started) {
            RunOutcome::Ran(report) => report,
            RunOutcome::NeedsApproval(preview) => panic!("unexpected approval: {preview:?}"),
        }
    }

    fn undo_at(&mut self, started: std::time::SystemTime) -> UndoReport {
        let journal = self.journal.clone().expect("a journaled run");
        let prepared = self.prepare();
        let mut store = self.store();
        prepared
            .undo(
                journal,
                started,
                &mut |planned, commit| store.apply(planned, commit),
                &mut proceed,
            )
            .unwrap()
    }
}

/// PLAN.md §3.6: a Backup target that cannot hold `.foldover` gets
/// nothing of Foldover's: no identity, lease or archive. What a run
/// replaces there is deleted, leftovers too, and undo is refused.
#[test]
fn a_backup_target_without_records_keeps_nothing_of_foldovers() {
    for kind in [RightKind::Local, RightKind::Sftp] {
        if kind == RightKind::Sftp && sftp_server().is_none() {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            continue;
        }
        backup_without_records(kind);
    }

    // Kept in both folders, the records stop the first run before it
    // writes anything.
    let mut both = fixture(SyncMode::Backup { source: Side::Left });
    fs::write(both.right.path().join(".foldover"), b"").unwrap();
    fs::write(both.left.path().join("file"), b"x").unwrap();
    let prepared = both.prepare();
    let mut store = both.store();
    let refused = prepared.execute(
        Approval::default(),
        RunContext::Reviewed,
        STARTED,
        &mut |planned, commit| store.apply(planned, commit),
        &mut proceed,
        &NoProgress,
    );
    assert!(matches!(
        refused,
        Err(RunError::IdentityWrite {
            side: Side::Right,
            ..
        })
    ));
    assert!(!both.right.path().join("file").exists());
}

fn backup_without_records(kind: RightKind) {
    let mut backup = fixture(SyncMode::Backup { source: Side::Left });
    backup.records = foldover_types::FolderRecords::SourceOnly;
    backup.right_kind = kind;
    let target = backup.right.path().to_owned();
    // A file where `.foldover` would go: the folder cannot hold one.
    fs::write(target.join(".foldover"), b"").unwrap();
    fs::write(backup.left.path().join("edited"), b"first").unwrap();
    let report = backup.sync();
    assert_eq!(report.copied(), 1);
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert!(backup.identities.side(Side::Right).is_some());

    fs::write(backup.left.path().join("edited"), b"second version").unwrap();
    fs::write(target.join(".foldover-tmp-deadbeef"), b"left behind").unwrap();
    let report = backup.sync();
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!((report.copied(), report.recovered()), (1, 1));
    assert_eq!(fs::read(target.join("edited")).unwrap(), b"second version");
    let mut names: Vec<_> = fs::read_dir(&target)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, [".foldover", "edited"]);
    assert_eq!(fs::read(target.join(".foldover")).unwrap(), b"");

    let journal = backup.journal.clone().expect("a journaled run");
    let prepared = backup.prepare();
    let mut store = backup.store();
    let undo = prepared.undo(
        journal,
        STARTED,
        &mut |planned, commit| store.apply(planned, commit),
        &mut proceed,
    );
    assert!(matches!(
        undo,
        Err(RunError::UndoUnavailable { side: Side::Right })
    ));
}

/// PLAN.md §4.7: a file one folder moved arrives in the other as a
/// rename, not a copy and an archive, and undo moves it back.
#[test]
fn moved_files_are_renamed_on_the_other_side_and_undo_moves_them_back() {
    use std::os::unix::fs::MetadataExt;

    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::create_dir(left.join("photos")).unwrap();
    fs::write(left.join("photos/a.jpg"), b"a").unwrap();
    fs::write(left.join("photos/b.jpg"), b"b").unwrap();
    pair.sync();
    let inode = fs::metadata(right.join("photos/a.jpg")).unwrap().ino();

    fs::create_dir(left.join("album")).unwrap();
    fs::rename(left.join("photos/a.jpg"), left.join("album/a.jpg")).unwrap();
    let preview = pair.prepare().preview();
    let moved: Vec<_> = preview
        .items()
        .iter()
        .filter(|item| matches!(item.action(), Action::Move { on: Side::Right }))
        .map(|item| (item.path().clone(), item.moved_from().cloned()))
        .collect();
    assert_eq!(moved, [(path(b"album/a.jpg"), Some(path(b"photos/a.jpg")))]);
    assert_eq!(preview.deletions(Side::Right).deletions(), 0);

    let report = pair.ran(Approval::default(), at(1));
    assert!(report.failures().is_empty(), "{:?}", report.failures());
    assert_eq!((report.moved(), report.archived()), (1, 0));
    assert_eq!(
        fs::metadata(right.join("album/a.jpg")).unwrap().ino(),
        inode
    );
    assert!(!right.join("photos/a.jpg").exists());
    assert!(pair.prepare().preview().items().is_empty());

    let undone = pair.undo_at(at(2));
    assert!(undone.not_undone().is_empty(), "{:?}", undone.not_undone());
    assert_eq!(
        fs::metadata(right.join("photos/a.jpg")).unwrap().ino(),
        inode
    );
    assert!(!right.join("album/a.jpg").exists());
}

/// Review of moves: a move that also brought new metadata went back
/// with it, as undo leaves metadata, but the old path got its base from
/// before the run; the next preview called the moved-back file edited
/// on one side and deleted on the other.
#[test]
fn undoing_a_move_that_brought_new_metadata_leaves_no_conflict() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("a.txt"), b"a").unwrap();
    pair.sync();

    // Same content, a new modification time and a new name.
    let edited = std::time::SystemTime::now() - Duration::from_secs(3_600);
    fs::File::options()
        .write(true)
        .open(left.join("a.txt"))
        .unwrap()
        .set_modified(edited)
        .unwrap();
    fs::rename(left.join("a.txt"), left.join("b.txt")).unwrap();
    let report = pair.ran(Approval::default(), at(1));
    assert_eq!(
        (report.moved(), report.copied()),
        (1, 0),
        "{:?}",
        report.failures()
    );

    let undone = pair.undo_at(at(2));
    assert!(undone.not_undone().is_empty(), "{:?}", undone.not_undone());
    assert!(right.join("a.txt").exists());

    let preview = pair.prepare().preview();
    let actions: Vec<_> = preview.items().iter().map(|item| item.action()).collect();
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Conflict(_))),
        "{actions:?}"
    );
    // Syncing again redoes the move.
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::Move { on: Side::Right })),
        "{actions:?}"
    );
}

#[test]
fn undo_reverts_the_newest_run_and_the_next_preview_shows_the_redo() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    fs::write(left.join("gone.txt"), b"g").unwrap();
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    let before = pair.base.clone();

    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    fs::remove_file(left.join("gone.txt")).unwrap();
    fs::write(left.join("new.txt"), b"n").unwrap();
    fs::create_dir(left.join("folder")).unwrap();
    fs::write(left.join("folder/x.txt"), b"x").unwrap();
    let report = pair.ran(Approval::new(0, 1), at(1));
    assert_eq!((report.copied(), report.archived()), (4, 1));

    let undo = pair.undo_at(at(2));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.removed(), undo.restored(), undo.beside()), (3, 2, 0));
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"v1");
    assert_eq!(fs::read(right.join("gone.txt")).unwrap(), b"g");
    assert!(!right.join("new.txt").exists());
    assert!(!right.join("folder").exists());
    // What the run created waits in the undo's own archive.
    let archive = right.join(".foldover/archive").join(undo.archive());
    assert_eq!(fs::read(archive.join("new.txt")).unwrap(), b"n");
    assert_eq!(fs::read(archive.join("folder/x.txt")).unwrap(), b"x");

    // The base is back to before the run, and the journal is spent.
    assert_eq!(pair.base, before);
    assert!(pair.journal.is_none());
    // Syncing again would redo exactly what was undone.
    assert_eq!(pair.prepare().preview().items().len(), 5);
}

#[test]
fn undo_leaves_entries_changed_since_the_run_and_says_why() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("new.txt"), b"n").unwrap();
    pair.ran(Approval::default(), at(1));
    fs::write(right.join("new.txt"), b"edited in B").unwrap();

    let undo = pair.undo_at(at(2));
    assert_eq!(undo.removed(), 0);
    assert_eq!(undo.not_undone().len(), 1);
    assert_eq!(undo.not_undone()[0].path(), &path(b"new.txt"));
    assert!(undo.not_undone()[0].message().starts_with("Changed since"));
    assert_eq!(fs::read(right.join("new.txt")).unwrap(), b"edited in B");
    assert!(pair.journal.is_none());
}

#[test]
fn undo_puts_a_removed_folder_back_with_its_files() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep.txt"), b"k").unwrap();
    fs::create_dir(left.join("dir")).unwrap();
    fs::write(left.join("dir/a"), b"a").unwrap();
    fs::write(left.join("dir/b"), b"b").unwrap();
    pair.sync();
    let before = pair.base.clone();
    fs::remove_dir_all(left.join("dir")).unwrap();
    assert_eq!(pair.ran(Approval::new(0, 3), at(1)).archived(), 3);
    assert!(!right.join("dir").exists());

    let undo = pair.undo_at(at(2));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!(undo.restored(), 3);
    assert_eq!(fs::read(right.join("dir/a")).unwrap(), b"a");
    assert_eq!(fs::read(right.join("dir/b")).unwrap(), b"b");
    assert_eq!(pair.base, before);
}

#[test]
fn undo_keeps_the_newer_version_when_the_earlier_one_left_the_archive() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    pair.sync();
    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    let report = pair.ran(Approval::default(), at(1));
    let archived = right
        .join(".foldover/archive")
        .join(report.archive())
        .join("edit.txt");
    fs::remove_file(archived).unwrap();

    let undo = pair.undo_at(at(2));
    assert_eq!(undo.restored(), 0);
    assert_eq!(
        undo.not_undone()[0].message(),
        "The earlier version is no longer in the archive"
    );
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"version 2");
}

/// Review: base restores committed after the reverts could be lost to a
/// crash, leaving a base that makes the next sync propagate the undo.
/// They now commit first; a crash right after that leaves both sides as
/// the run left them under their pre-run base, which plans no writes.
#[test]
fn a_crash_after_an_undo_commits_its_base_plans_no_writes_and_undo_can_retry() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("edit.txt"), b"v1").unwrap();
    fs::write(left.join("gone.txt"), b"g").unwrap();
    fs::write(left.join("keep.txt"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("edit.txt"), b"version 2").unwrap();
    fs::remove_file(left.join("gone.txt")).unwrap();
    fs::write(left.join("new.txt"), b"n").unwrap();
    pair.ran(Approval::new(0, 1), at(1));

    let journal = pair.journal.clone().unwrap();
    let prepared = pair.prepare();
    let mut store = pair.store();
    let error = prepared
        .undo(
            journal,
            at(2),
            &mut |planned, commit| {
                let undoing = matches!(commit, Commit::Undoing { .. });
                store.apply(planned, commit)?;
                if undoing {
                    // The agent dies right after the base commit.
                    return Err(crate::Error::StaleRevision);
                }
                Ok(())
            },
            &mut proceed,
        )
        .unwrap_err();
    assert!(matches!(error, RunError::Commit(_)), "{error:?}");
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"version 2");
    assert!(right.join("new.txt").exists() && !right.join("gone.txt").exists());
    assert!(pair.prepare().preview().items().is_empty());

    let undo = pair.undo_at(at(3));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.removed(), undo.restored()), (1, 2));
    assert_eq!(fs::read(right.join("edit.txt")).unwrap(), b"v1");
}

/// Review: a retried undo planned every entry again, so what a cancelled
/// attempt had already put back was listed as no longer archived.
#[test]
fn undoing_again_after_a_cancel_picks_up_only_what_is_left() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    let count = COMMIT_BATCH + 1;
    fs::write(left.join("keep.txt"), b"k").unwrap();
    for index in 0..count {
        fs::write(left.join(format!("{index}.txt")), b"x").unwrap();
    }
    pair.sync();
    let before = pair.base.clone();
    for index in 0..count {
        fs::remove_file(left.join(format!("{index}.txt"))).unwrap();
    }
    pair.ran(Approval::new(0, count as u64), at(1));

    // Cancelled once the first batch is back.
    let journal = pair.journal.clone().unwrap();
    let prepared = pair.prepare();
    let mut store = pair.store();
    let mut checks = 0;
    let undo = prepared
        .undo(
            journal,
            at(2),
            &mut |planned, commit| store.apply(planned, commit),
            &mut || {
                checks += 1;
                if checks > 1 {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            },
        )
        .unwrap();
    assert_eq!(undo.completion(), Completion::Cancelled);
    assert_eq!(undo.restored(), COMMIT_BATCH as u64);

    let undo = pair.undo_at(at(3));
    assert!(undo.not_undone().is_empty(), "{:?}", undo.not_undone());
    assert_eq!((undo.restored(), undo.beside()), (1, 0));
    // Every file, keep.txt and .foldover.
    assert_eq!(fs::read_dir(&right).unwrap().count(), count + 2);
    assert_eq!(pair.base, before);
    assert!(pair.journal.is_none());
}

impl Fixture {
    /// The versions a fresh preview shows for `name`.
    fn previewed(&self, name: &[u8]) -> (Option<EntryVersion>, Option<EntryVersion>) {
        self.prepare()
            .preview()
            .items()
            .iter()
            .find(|item| item.path() == &path(name))
            .unwrap()
            .versions()
    }

    fn keep(&mut self, side: Side, name: &[u8]) {
        let (left, right) = self.previewed(name);
        self.resolutions.insert(
            path(name),
            foldover_types::Resolution::new(side, left, right),
        );
    }
}

/// PLAN.md §3.8 per-item override: keeping the side a change would have
/// overwritten copies it the other way instead.
#[test]
fn keeping_the_target_side_copies_a_change_the_other_way() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("deleted"), b"d").unwrap();
    fs::write(left.join("edited"), b"v1").unwrap();
    fs::write(left.join("other"), b"o").unwrap();
    pair.sync();
    fs::remove_file(left.join("deleted")).unwrap();
    fs::write(left.join("edited"), b"version 2").unwrap();

    pair.keep(Side::Right, b"deleted");
    pair.keep(Side::Right, b"edited");
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (2, 0));
    // The deletion is undone in A instead of propagated to B.
    assert_eq!(fs::read(left.join("deleted")).unwrap(), b"d");
    // B's version wins in A; A's edit is in A's archive.
    assert_eq!(fs::read(left.join("edited")).unwrap(), b"v1");
    let archive = left.join(".foldover/archive/1970-01-01T00-00-00.000Z");
    assert_eq!(fs::read(archive.join("edited")).unwrap(), b"version 2");
    assert_eq!(fs::read(right.join("edited")).unwrap(), b"v1");
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn keeping_a_folder_a_deletion_would_archive_restores_it_with_its_files() {
    let mut pair = fixture(SyncMode::Sync);
    let left = pair.left.path().to_owned();
    fs::write(left.join("keep"), b"k").unwrap();
    fs::create_dir(left.join("dir")).unwrap();
    fs::write(left.join("dir/a"), b"a").unwrap();
    pair.sync();
    fs::remove_dir_all(left.join("dir")).unwrap();

    pair.keep(Side::Right, b"dir");
    let report = pair.sync();
    assert_eq!((report.copied(), report.archived()), (2, 0));
    assert_eq!(fs::read(left.join("dir/a")).unwrap(), b"a");
    assert!(pair.prepare().preview().items().is_empty());
}

#[test]
fn a_skipped_change_and_everything_inside_it_waits_for_another_run() {
    let mut pair = fixture(SyncMode::Sync);
    let (left, right) = (pair.left.path().to_owned(), pair.right.path().to_owned());
    fs::write(left.join("keep"), b"k").unwrap();
    pair.sync();
    fs::write(left.join("later"), b"l").unwrap();
    fs::write(left.join("now"), b"n").unwrap();
    fs::create_dir(left.join("folder")).unwrap();
    fs::write(left.join("folder/inner"), b"i").unwrap();

    for name in [b"later".as_slice(), b"folder"] {
        let (left, right) = pair.previewed(name);
        pair.skips
            .insert(path(name), foldover_types::Skip::new(left, right));
    }
    let preview = pair.prepare().preview();
    assert!(
        preview
            .items()
            .iter()
            .filter(|item| item.path() != &path(b"now"))
            .all(|item| item.action() == Action::KeepTarget),
        "{preview:?}"
    );
    assert_eq!(pair.sync().copied(), 1);
    assert!(right.join("now").exists());
    assert!(!right.join("later").exists() && !right.join("folder").exists());

    // A skip covers what the person saw; an edit since makes it lapse.
    fs::write(left.join("later"), b"edited").unwrap();
    assert_eq!(pair.sync().copied(), 1);
    assert!(right.join("later").exists());

    // Without the skip the rest copies on the next run.
    pair.skips.clear();
    assert_eq!(pair.sync().copied(), 2);
    assert!(right.join("folder/inner").exists());
}

/// Counts what a run reports, as the agent's status does.
#[derive(Default)]
struct Counts {
    listed: std::cell::Cell<u64>,
    checked: std::cell::Cell<u64>,
    planned: std::cell::Cell<(u64, u64)>,
    applied: std::cell::Cell<u64>,
    copied: std::cell::Cell<u64>,
}

impl RunProgress for Counts {
    fn waiting(&self, _: Option<&str>) {}

    fn observed(&self, _: Side, step: ObserveStep) {
        let counter = match step {
            ObserveStep::Listed => &self.listed,
            ObserveStep::Checked => &self.checked,
        };
        counter.set(counter.get() + 1);
    }

    fn planned(&self, changes: u64, bytes: u64) {
        self.planned.set((changes, bytes));
    }

    fn applied(&self, bytes: u64) {
        self.applied.set(self.applied.get() + 1);
        self.copied.set(self.copied.get() + bytes);
    }
}

#[test]
fn runs_report_what_they_observe_plan_and_apply() {
    let pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("a"), b"aaa").unwrap();
    fs::create_dir(pair.left.path().join("dir")).unwrap();
    fs::write(pair.left.path().join("dir/b"), b"bbbbb").unwrap();
    fs::write(pair.right.path().join("c"), b"c").unwrap();

    let counts = Counts::default();
    let prepared = PairRun::prepare(pair.inputs(), &mut proceed, &counts).unwrap();
    assert_eq!((counts.listed.get(), counts.checked.get()), (4, 4));

    let outcome = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, _| Ok(()),
            &mut proceed,
            &counts,
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Ran(_)));
    // Four changes: a, dir and dir/b to B; c to A. Folders copy no bytes.
    assert_eq!(counts.planned.get(), (4, 9));
    assert_eq!((counts.applied.get(), counts.copied.get()), (4, 9));
}

/// Review: with only one identity recorded (the other root was changed),
/// the recorded side must still be checked, not adopted, before writes.
#[test]
fn a_recorded_side_must_still_match_when_the_other_side_is_new() {
    let mut pair = fixture(SyncMode::Sync);
    fs::write(pair.left.path().join("file"), b"x").unwrap();
    pair.sync();
    let left = pair.identities.side(Side::Left).unwrap();
    pair.identities = RecordedIdentities::new(Some(left), None);
    pair.right = tempfile::tempdir().unwrap();

    let prepared = pair.prepare();
    // The left disk is swapped while the scan runs.
    let other = EndpointId::from_bytes([9; 16]);
    fs::write(pair.left.path().join(".foldover/id"), format!("{other}\n")).unwrap();
    let error = prepared
        .execute(
            Approval::default(),
            RunContext::Reviewed,
            STARTED,
            &mut |_, commit| {
                assert!(!matches!(commit, Commit::Identities { .. }));
                Ok(())
            },
            &mut proceed,
            &NoProgress,
        )
        .unwrap_err();
    assert!(
        matches!(error, RunError::IdentityChanged { side: Side::Left }),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(pair.right.path()).unwrap().count(), 0);
}

mod simulation;

/// Review of #84: a canary kept unchanged past the pair's age limit was
/// skipped as old, and every run then stopped as if it were missing.
#[test]
fn an_old_canary_still_proves_the_folders() {
    let mut pair = fixture(SyncMode::Sync);
    pair.canary = Some(path(b"canary"));
    let hundred_days = std::time::Duration::from_secs(100 * 24 * 60 * 60);
    for folder in [pair.left.path(), pair.right.path()] {
        fs::write(folder.join("canary"), b"here").unwrap();
        fs::File::options()
            .write(true)
            .open(folder.join("canary"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - hundred_days)
            .unwrap();
    }
    pair.age_limit = Some(30);
    pair.sync();
    fs::write(pair.left.path().join("new"), b"n").unwrap();
    assert_eq!(pair.sync().copied(), 1);

    // Review: exempt from the age limit, a canary that is gone still
    // stops the run.
    fs::remove_file(pair.right.path().join("canary")).unwrap();
    let Err(error) = pair.try_prepare().and_then(|prepared| {
        prepared.execute(
            Approval::default(),
            RunContext::Interactive,
            STARTED,
            &mut |_, _| panic!("nothing may commit"),
            &mut proceed,
            &NoProgress,
        )
    }) else {
        panic!("a run without its canary went ahead");
    };
    assert!(
        matches!(
            error,
            RunError::CanaryMissing {
                side: Side::Right,
                ..
            }
        ),
        "{error:?}"
    );
}

/// Review of #84: a canary a `.foldoverignore` file leaves out was
/// reported missing, sending people to look for a file that is there.
#[test]
fn a_canary_left_out_by_an_ignore_file_says_so() {
    let mut pair = fixture(SyncMode::Sync);
    pair.canary = Some(path(b"canary"));
    for folder in [pair.left.path(), pair.right.path()] {
        fs::write(folder.join("canary"), b"here").unwrap();
    }
    fs::write(pair.left.path().join(".foldoverignore"), b"canary\n").unwrap();
    // Even a preview says so, before anything is compared.
    let Err(error) = pair.try_prepare() else {
        panic!("a run with an excluded canary was prepared");
    };
    assert!(
        matches!(&error, RunError::CanaryExcluded { path } if *path == super::tests::path(b"canary")),
        "{error:?}"
    );
}
