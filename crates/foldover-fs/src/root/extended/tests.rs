use super::*;
use std::fs;
use std::os::unix::fs::symlink;

fn path() -> SyncPath {
    SyncPath::try_from(b"file".as_slice()).unwrap()
}

fn set(file: &impl AsFd, name: &std::ffi::CStr, value: &[u8]) {
    rustix::fs::fsetxattr(file, name, value, rustix::fs::XattrFlags::empty()).unwrap();
}

fn observed<'a>(extended: &'a ExtendedMetadata, name: &[u8]) -> &'a ExtendedAttribute {
    extended
        .attributes()
        .iter()
        .find(|attribute| attribute.name() == name)
        .unwrap()
}

#[test]
fn native_attributes_keep_empty_binary_values_and_exact_sorted_names() {
    let temporary = tempfile::tempdir().unwrap();
    let file = fs::File::create(temporary.path().join("file")).unwrap();
    for (name, value) in [
        (c"user.foldover.z", b"\0\xffbinary".as_slice()),
        (c"user.foldover.a", b""),
    ] {
        set(&file, name, value);
    }
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    let result = root
        .read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue)
        .unwrap();
    for (name, value) in [
        (b"user.foldover.z".as_slice(), b"\0\xffbinary".as_slice()),
        (b"user.foldover.a", b""),
    ] {
        let attribute = observed(&result, name);
        assert_eq!(attribute.size(), value.len() as u64);
        assert_eq!(
            attribute.digest().as_bytes(),
            blake3::hash(value).as_bytes()
        );
    }
    assert!(
        result
            .attributes()
            .windows(2)
            .all(|pair| pair[0].name() < pair[1].name())
    );
    #[cfg(target_os = "linux")]
    assert_eq!(result.access_control(), &AccessControl::NotReported);
    assert_eq!(
        result,
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue)
            .unwrap()
    );

    let directory = fs::File::open(temporary.path()).unwrap();
    set(&directory, c"user.foldover.root", b"root metadata");
    let metadata = root.root_metadata().unwrap();
    let result = root
        .read_root_extended_metadata(&metadata, &mut |_| ScanControl::Continue)
        .unwrap();
    assert_eq!(observed(&result, b"user.foldover.root").size(), 13);
}

#[test]
fn cancellation_mutations_and_stale_observations_never_produce_complete_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let file = fs::File::create(temporary.path().join("file")).unwrap();
    set(&file, c"user.foldover.test", b"old");
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Cancel),
        Err(MetadataReadError::Cancelled)
    ));
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |bytes| {
            if bytes == 0 {
                ScanControl::Continue
            } else {
                ScanControl::Cancel
            }
        }),
        Err(MetadataReadError::Cancelled)
    ));

    let mut changed = false;
    let result = root.read_extended_metadata(&path(), &metadata, &mut |bytes| {
        if bytes > 0 && !changed {
            changed = true;
            // `ctime` advances in coarse ticks on several filesystems, so an
            // xattr write made in the tick the read started in leaves the
            // stat key — the only thing this read compares before and after
            // — unchanged. Repeat it, one tick apart, until the key moves.
            // The bound and the step match `STAT_SETTLE_ATTEMPTS` and
            // `STAT_SETTLE_STEP` in foldover-endpoint's tests, which settle
            // the same way; both are named here so the numbers are not bare.
            const ATTEMPTS: u32 = 250;
            const STEP: std::time::Duration = std::time::Duration::from_millis(20);
            for _ in 0..ATTEMPTS {
                set(&file, c"user.foldover.test", b"new");
                if root.metadata(&path()).unwrap().stat_key() != metadata.stat_key() {
                    break;
                }
                std::thread::sleep(STEP);
            }
            assert_ne!(
                root.metadata(&path()).unwrap().stat_key(),
                metadata.stat_key(),
                "the filesystem never showed the xattr write in the stat key"
            );
        }
        ScanControl::Continue
    });
    assert!(changed);
    assert!(matches!(result, Err(MetadataReadError::Changed)));
    assert_eq!(
        metadata.modified(),
        root.metadata(&path()).unwrap().modified()
    );
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue),
        Err(MetadataReadError::Changed)
    ));
}

#[test]
fn swapped_paths_special_files_and_dataless_entries_are_never_read_as_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("file"), b"inside").unwrap();
    fs::write(outside.path().join("file"), b"outside").unwrap();
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    fs::remove_file(temporary.path().join("file")).unwrap();
    symlink(outside.path().join("file"), temporary.path().join("file")).unwrap();
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue),
        Err(MetadataReadError::Changed)
    ));
    symlink(outside.path(), temporary.path().join("escape")).unwrap();
    assert!(
        root.read_extended_metadata(
            &SyncPath::try_from(b"escape/file".as_slice()).unwrap(),
            &metadata,
            &mut |_| ScanControl::Continue
        )
        .is_err()
    );

    let mut dataless = metadata;
    dataless.residency = DataResidency::Dataless;
    assert!(matches!(
        root.read_extended_metadata(&path(), &dataless, &mut |_| ScanControl::Continue),
        Err(MetadataReadError::Dataless)
    ));
    fs::remove_file(temporary.path().join("file")).unwrap();
    nix::unistd::mkfifo(
        &temporary.path().join("file"),
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .unwrap();
    let metadata = root.metadata(&path()).unwrap();
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue),
        Err(MetadataReadError::UnsupportedEntry)
    ));
}

#[test]
fn held_metadata_access_does_not_switch_to_a_replacement_ambient_root() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = temporary.path().join("selected");
    fs::create_dir(&selected).unwrap();
    let file = fs::File::create(selected.join("file")).unwrap();
    set(&file, c"user.foldover.test", b"held");
    let root = Root::open(&selected).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    fs::rename(&selected, temporary.path().join("moved")).unwrap();
    fs::create_dir(&selected).unwrap();
    let replacement = fs::File::create(selected.join("file")).unwrap();
    set(&replacement, c"user.foldover.test", b"replacement");
    let result = root
        .read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue)
        .unwrap();
    assert_eq!(
        observed(&result, b"user.foldover.test").digest().as_bytes(),
        blake3::hash(b"held").as_bytes()
    );
}

#[test]
fn malformed_attribute_lists_and_oversized_values_fail_closed() {
    for bytes in [b"\0".as_slice(), b"unterminated", b"a\0a\0", b"a\0\0"] {
        assert_eq!(
            parse_names(bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert!(parse_names(b"").unwrap().is_empty());
    assert_eq!(
        parse_names(b"\xff\0a\0")
            .unwrap()
            .iter()
            .map(|name| name.as_bytes())
            .collect::<Vec<_>>(),
        [b"a".as_slice(), b"\xff"]
    );
    let file = tempfile::tempfile().unwrap();
    let mut control = |_| panic!("Oversized values must fail before reading");
    let mut progress = Progress {
        control: &mut control,
        bytes: 0,
    };
    assert!(matches!(
        attribute_digest(
            &file,
            c"user.oversized",
            MAX_ATTRIBUTE_VALUE_BYTES + 1,
            &mut progress
        ),
        Err(MetadataReadError::LimitExceeded)
    ));
    #[cfg(target_os = "macos")]
    assert!(matches!(
        resource_fork_digest(&file, MAX_RESOURCE_FORK_BYTES + 1, &mut progress),
        Err(MetadataReadError::LimitExceeded)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn unsupported_symlink_metadata_does_not_turn_into_empty_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    symlink("/outside", temporary.path().join("file")).unwrap();
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue),
        Err(MetadataReadError::UnsupportedEntry)
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn native_resource_forks_are_streamed_and_acl_changes_are_observed() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = temporary.path().join("file");
    fs::write(&selected, b"data fork").unwrap();
    let file = fs::File::open(&selected).unwrap();
    let value = vec![b'r'; ATTRIBUTE_BUFFER_BYTES * 2 + 17];
    set(&file, RESOURCE_FORK, &value);
    let mut finder_info = [0; 32];
    finder_info[..4].copy_from_slice(b"FOLD");
    set(&file, c"com.apple.FinderInfo", &finder_info);
    let tags = b"<?xml version=\"1.0\"?><plist version=\"1.0\"><array><string>Blue\n4</string></array></plist>";
    set(&file, c"com.apple.metadata:_kMDItemUserTags", tags);
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    let mut progress = Vec::new();
    let result = root
        .read_extended_metadata(&path(), &metadata, &mut |bytes| {
            progress.push(bytes);
            ScanControl::Continue
        })
        .unwrap();
    let fork = observed(&result, RESOURCE_FORK.to_bytes());
    assert_eq!(fork.size(), value.len() as u64);
    assert_eq!(fork.digest().as_bytes(), blake3::hash(&value).as_bytes());
    assert_eq!(
        observed(&result, b"com.apple.FinderInfo")
            .digest()
            .as_bytes(),
        blake3::hash(&finder_info).as_bytes()
    );
    assert_eq!(
        observed(&result, b"com.apple.metadata:_kMDItemUserTags")
            .digest()
            .as_bytes(),
        blake3::hash(tags).as_bytes()
    );
    assert!(
        progress
            .windows(2)
            .all(|pair| pair[1] - pair[0] <= ATTRIBUTE_BUFFER_BYTES as u64)
    );
    assert!(matches!(result.access_control(), AccessControl::MacOs(_)));
    assert!(matches!(
        root.read_extended_metadata(&path(), &metadata, &mut |bytes| if bytes
            > ATTRIBUTE_BUFFER_BYTES as u64
        {
            ScanControl::Cancel
        } else {
            ScanControl::Continue
        }),
        Err(MetadataReadError::Cancelled)
    ));

    let status = std::process::Command::new("/bin/chmod")
        .args([
            "+a",
            "everyone allow read,readattr,readextattr,readsecurity",
        ])
        .arg(&selected)
        .status()
        .unwrap();
    assert!(status.success());
    let changed = root.metadata(&path()).unwrap();
    assert_eq!(metadata.modified(), changed.modified());
    let updated = root
        .read_extended_metadata(&path(), &changed, &mut |_| ScanControl::Continue)
        .unwrap();
    assert_ne!(result.access_control(), updated.access_control());
    assert!(
        matches!(updated.access_control(), AccessControl::MacOs(Some(bytes)) if !bytes.is_empty())
    );
    assert!(
        std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(selected)
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_link_metadata_belongs_to_the_link_not_its_outside_target() {
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    set(outside.as_file(), c"user.foldover.outside", b"private");
    symlink(outside.path(), temporary.path().join("file")).unwrap();
    rustix::fs::lsetxattr(
        temporary.path().join("file"),
        c"user.foldover.link",
        b"link",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let root = Root::open(temporary.path()).unwrap();
    let metadata = root.metadata(&path()).unwrap();
    let result = root
        .read_extended_metadata(&path(), &metadata, &mut |_| ScanControl::Continue)
        .unwrap();
    assert_eq!(observed(&result, b"user.foldover.link").size(), 4);
    assert!(
        !result
            .attributes()
            .iter()
            .any(|attribute| attribute.name() == b"user.foldover.outside")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn changing_resource_forks_cannot_extend_the_observed_work_or_publish_a_digest() {
    let temporary = tempfile::tempdir().unwrap();
    let file = fs::File::create(temporary.path().join("file")).unwrap();
    let root = Root::open(temporary.path()).unwrap();
    let original = vec![b'x'; ATTRIBUTE_BUFFER_BYTES * 2 + 17];
    for replacement in [
        vec![b'y'; ATTRIBUTE_BUFFER_BYTES],
        vec![b'y'; ATTRIBUTE_BUFFER_BYTES * 4],
    ] {
        set(&file, RESOURCE_FORK, &original);
        let metadata = root.metadata(&path()).unwrap();
        let mut changed = false;
        let mut maximum = 0;
        let result = root.read_extended_metadata(&path(), &metadata, &mut |bytes| {
            maximum = maximum.max(bytes);
            if bytes >= ATTRIBUTE_BUFFER_BYTES as u64 && !changed {
                set(&file, RESOURCE_FORK, &replacement);
                changed = true;
            }
            ScanControl::Continue
        });
        assert!(changed);
        assert!(matches!(result, Err(MetadataReadError::Changed)));
        assert!(maximum <= original.len() as u64);
    }
}
