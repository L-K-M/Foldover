//! Regression tests for the private local catalog primitives and for
//! `CatalogFiles` over a local root: exclusive creation, bounded reads,
//! invalid names, symlink/directory injection, parent symlinks, the
//! directory marker, and the metadata lease's lifetime.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use foldover_types::ScanControl;

use super::*;
use crate::{CATALOG_FILE_LIMIT, CatalogFiles, Endpoint, HeldLease, LeaseHolder, LeaseTiming};

fn pair() -> PairId {
    crate::test_pair()
}

fn state_directory(root: &Path) -> std::path::PathBuf {
    root.join(".foldover/state").join(format!("{}", pair()))
}

/// A local catalog over a fresh temporary root.
fn catalog(root: &Path) -> LocalEndpoint {
    LocalEndpoint::open(root)
        .unwrap()
        .catalog_endpoint(pair(), Durability::Standard)
        .unwrap()
}

#[test]
fn local_catalog_roundtrip_read_list_remove() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());

    assert_eq!(catalog.catalog_read("missing", 64).unwrap(), None);
    catalog
        .catalog_write("alpha", b"one", Durability::Standard)
        .unwrap();
    catalog
        .catalog_write("beta", b"two", Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
    assert_eq!(
        catalog.catalog_read("beta", 64).unwrap().as_deref(),
        Some(b"two".as_slice())
    );

    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "format"]);

    catalog.catalog_remove("alpha").unwrap();
    assert_eq!(catalog.catalog_read("alpha", 64).unwrap(), None);
    // Removing what is already gone is not an error.
    catalog.catalog_remove("alpha").unwrap();
    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["beta", "format"]);
}

#[test]
fn local_catalog_create_is_exclusive_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    catalog
        .catalog_write("alpha", b"one", Durability::Standard)
        .unwrap();

    let error = catalog
        .catalog_write("alpha", b"two", Durability::Standard)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        catalog.catalog_read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );

    // A directory at the name is not replaced either.
    let occupied = state_directory(root.path()).join("occupied");
    fs::create_dir(&occupied).unwrap();
    let error = catalog
        .catalog_write("occupied", b"three", Durability::Standard)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert!(occupied.is_dir());

    // `CatalogFiles` shares the same exclusive create.
    let files = local_files(root.path());
    let error = files.write_new("alpha", b"four").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        files.read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
}

#[test]
fn local_catalog_read_is_bounded_and_rejects_invalid_names() {
    let root = tempfile::tempdir().unwrap();
    let catalog = catalog(root.path());
    let bytes = vec![b'x'; 32];
    catalog
        .catalog_write("big", &bytes, Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("big", 64).unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    let error = catalog.catalog_read("big", 8).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let long = "x".repeat(129);
    for name in [
        "",
        ".hidden",
        "UPPER",
        "a/b",
        "a\\b",
        "space name",
        long.as_str(),
    ] {
        assert!(
            catalog
                .catalog_write(name, b"x", Durability::Standard)
                .is_err(),
            "write accepted {name:?}"
        );
        assert!(
            catalog.catalog_read(name, 64).is_err(),
            "read accepted {name:?}"
        );
        assert!(
            catalog.catalog_remove(name).is_err(),
            "remove accepted {name:?}"
        );
    }
}

#[test]
fn local_catalog_refuses_symlink_and_directory_injection() {
    let root = tempfile::tempdir().unwrap();
    let state = state_directory(root.path());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("format"), DIRECTORY_MARKER).unwrap();
    let outside = root.path().join("outside");
    fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, state.join("link")).unwrap();
    fs::create_dir(state.join("subdir")).unwrap();
    let catalog = LocalEndpoint::open(root.path())
        .unwrap()
        .catalog_endpoint(pair(), Durability::Standard)
        .unwrap();

    // Neither a link nor a folder is read as a catalog file.
    assert!(catalog.catalog_read("link", 64).is_err());
    assert!(catalog.catalog_read("subdir", 64).is_err());
    // Cleanup refuses them too, rather than unlinking through them.
    assert_eq!(
        catalog.catalog_remove("link").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        catalog.catalog_remove("subdir").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // Writing over either is an exclusive-create refusal, not a replacement.
    assert_eq!(
        catalog
            .catalog_write("link", b"x", Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        catalog
            .catalog_write("subdir", b"x", Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    // The listing keeps only regular catalog files.
    let mut names = catalog.catalog_list().unwrap();
    names.sort();
    assert_eq!(names, ["format"]);
    assert_eq!(fs::read(&outside).unwrap(), b"secret");
}

#[test]
fn local_catalog_refuses_parent_symlink() {
    for link in [".foldover", ".foldover/state"] {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = root.path().join(link);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &target).unwrap();

        let error = LocalEndpoint::open(root.path())
            .unwrap()
            .catalog_endpoint(pair(), Durability::Standard)
            .map(|_| ())
            .unwrap_err();
        assert!(error.kind() != io::ErrorKind::NotFound, "{link}: {error:?}");
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            0,
            "{link}: wrote through the symlink"
        );
    }
}

#[test]
fn local_catalog_marker_mismatch_refuses_directory() {
    let root = tempfile::tempdir().unwrap();
    let state = state_directory(root.path());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("format"), b"not the marker").unwrap();
    let error = LocalEndpoint::open(root.path())
        .unwrap()
        .catalog_endpoint(pair(), Durability::Standard)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // A directory at the marker name is not a readable marker either.
    fs::remove_file(state.join("format")).unwrap();
    fs::create_dir(state.join("format")).unwrap();
    let error = LocalEndpoint::open(root.path())
        .unwrap()
        .catalog_endpoint(pair(), Durability::Standard)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

/// A `CatalogFiles` over a fresh local root.
fn local_files(root: &Path) -> CatalogFiles {
    let endpoint = Arc::new(Endpoint::Local(LocalEndpoint::open(root).unwrap()));
    let holder = LeaseHolder {
        id: "catalog-tests".into(),
        label: "Catalog tests".into(),
    };
    let mut control = || ScanControl::Continue;
    CatalogFiles::open(endpoint, pair(), holder, Durability::Standard, &mut control).unwrap()
}

#[test]
fn catalog_files_roundtrip_and_lease_lifetime() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = Arc::new(Endpoint::Local(LocalEndpoint::open(root.path()).unwrap()));
    let holder = LeaseHolder {
        id: "catalog-tests".into(),
        label: "Catalog tests".into(),
    };
    let mut control = || ScanControl::Continue;
    assert_eq!(endpoint.identity().unwrap(), None);

    let files = CatalogFiles::open(
        Arc::clone(&endpoint),
        pair(),
        holder,
        Durability::Standard,
        &mut control,
    )
    .unwrap();
    assert_eq!(files.identity(), endpoint.identity().unwrap().unwrap());

    // The metadata lease lives under the catalog directory, not the content
    // root's `.foldover/lock`.
    let catalog_lock = state_directory(root.path()).join(".foldover/lock");
    assert!(catalog_lock.exists());
    assert!(!root.path().join(".foldover/lock").exists());

    files.write_new("alpha", b"one").unwrap();
    files.write_new("beta", b"two").unwrap();
    assert_eq!(
        files.read("alpha", 64).unwrap().as_deref(),
        Some(b"one".as_slice())
    );
    assert_eq!(files.read("missing", 64).unwrap(), None);
    let mut names = files.list().unwrap();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "format"]);
    assert_eq!(
        files
            .read("alpha", CATALOG_FILE_LIMIT + 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    // The content root's own lease is a different file: one run can hold it
    // while the catalog lease is alive.
    let content = HeldLease::take(
        &endpoint,
        LeaseHolder {
            id: "content-tests".into(),
            label: "Content tests".into(),
        },
        LeaseTiming::STANDARD,
        &mut control,
        &mut |_| {},
    )
    .unwrap();
    assert!(root.path().join(".foldover/lock").exists());
    assert_eq!(
        files.read("beta", 64).unwrap().as_deref(),
        Some(b"two".as_slice())
    );
    files.fence().unwrap();
    drop(content);
    assert!(!root.path().join(".foldover/lock").exists());

    files.remove("alpha").unwrap();
    assert_eq!(files.read("alpha", 64).unwrap(), None);

    drop(files);
    assert!(
        !catalog_lock.exists(),
        "the catalog lease is released on drop"
    );
}
