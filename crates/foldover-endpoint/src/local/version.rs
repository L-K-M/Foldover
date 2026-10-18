//! Canonical local entry versions: what the planner compares and the base
//! keeps per side.
//!
//! ```text
//! content  = file: data digest + resource fork (if any)
//!            symlink: target bytes
//! metadata = the pair's synced categories: file mtime; xattrs and the
//!            hidden/no-dump flags; mode bits and ACL (else the file's
//!            executable bit)
//! ```
//!
//! Content must match across endpoints; metadata is recorded per side, so a
//! target that cannot store a field never causes a copy loop.

use std::time::{SystemTime, UNIX_EPOCH};

use foldover_fs::{AccessControl, NodeKind};
use foldover_types::{Digest, EntryContent, EntryVersion, MetadataPolicy, Propagation};

// Domain separation keeps content, link and metadata digests distinct from
// each other and from raw data hashes.
const FILE_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local file content v1";
const LINK_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local symlink target v1";
const METADATA_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local entry metadata v1";

/// Part of file content, so metadata-only conflict policy cannot drop fork edits.
const RESOURCE_FORK: &[u8] = b"com.apple.ResourceFork";
/// Compression storage. Decompressed reads already cover the data.
const COMPRESSION: &[u8] = b"com.apple.decmpfs";
/// Maintained by macOS per Mac or per access; never user content.
const SYSTEM_MANAGED: &[&[u8]] = &[
    b"com.apple.lastuseddate#PS",
    b"com.apple.macl",
    b"com.apple.provenance",
    b"com.apple.rootless",
];

const PERMISSION_BITS: u32 = 0o7777;
const EXECUTE_BITS: u32 = 0o111;

pub(super) enum Payload<'a> {
    File(Digest),
    Directory,
    Symlink(&'a [u8]),
}

#[derive(Clone, Copy)]
pub(super) struct Attribute<'a> {
    pub(super) name: &'a [u8],
    pub(super) size: u64,
    pub(super) digest: Digest,
}

/// The observed fields a version depends on. `attributes` is `None` only where
/// the platform cannot report them (Linux symlinks), never after a failed read.
pub(super) struct Observed<'a> {
    pub(super) kind: NodeKind,
    pub(super) permissions: u32,
    pub(super) modified: SystemTime,
    pub(super) user_flags: Option<u32>,
    pub(super) compressed: bool,
    pub(super) attributes: Option<Vec<Attribute<'a>>>,
    pub(super) access_control: Option<&'a AccessControl>,
}

pub(super) fn entry_version(
    payload: Payload<'_>,
    observed: &Observed<'_>,
    policy: MetadataPolicy,
) -> EntryVersion {
    let attributes = observed.attributes.as_deref().unwrap_or_default();
    let fork = match payload {
        Payload::File(_) if !observed.compressed => attributes
            .iter()
            .find(|attribute| attribute.name == RESOURCE_FORK),
        _ => None,
    };
    let content = match payload {
        Payload::File(data) => EntryContent::File(file_content(data, fork)),
        Payload::Directory => EntryContent::Directory,
        Payload::Symlink(target) => {
            let mut hasher = blake3::Hasher::new_derive_key(LINK_CONTEXT);
            hasher.update(target);
            EntryContent::Symlink(finish(&hasher))
        }
    };
    EntryVersion::new(content, metadata_digest(observed, policy))
}

fn file_content(data: Digest, fork: Option<&Attribute<'_>>) -> Digest {
    let mut hasher = blake3::Hasher::new_derive_key(FILE_CONTEXT);
    hasher.update(data.as_bytes());
    match fork {
        None => {
            hasher.update(&[0]);
        }
        Some(fork) => {
            hasher.update(&[1]);
            hasher.update(&fork.size.to_le_bytes());
            hasher.update(fork.digest.as_bytes());
        }
    }
    finish(&hasher)
}

fn metadata_digest(observed: &Observed<'_>, policy: MetadataPolicy) -> Digest {
    let mut hasher = blake3::Hasher::new_derive_key(METADATA_CONTEXT);

    // Link modes are not meaningful on Linux and not portable elsewhere.
    match (policy.permissions(), observed.kind) {
        (_, NodeKind::Symlink) => {}
        (Propagation::Sync, _) => {
            hasher
                .update(&[1])
                .update(&(observed.permissions & PERMISSION_BITS).to_le_bytes());
            update_access_control(&mut hasher, observed.access_control);
        }
        (Propagation::Ignore, NodeKind::File) => {
            let executable = observed.permissions & EXECUTE_BITS != 0;
            hasher.update(&[2, u8::from(executable)]);
        }
        (Propagation::Ignore, _) => {
            hasher.update(&[0]);
        }
    }

    // Directory mtimes move with every child change, and link mtimes are
    // rarely settable; tracking either would create metadata churn.
    match (policy.modification_times(), observed.kind) {
        (Propagation::Sync, NodeKind::File) => {
            hasher.update(&[1]);
            update_time(&mut hasher, observed.modified);
        }
        _ => {
            hasher.update(&[0]);
        }
    }

    if policy.extended_attributes() == Propagation::Ignore {
        hasher.update(&[0]);
        return finish(&hasher);
    }
    hasher.update(&[1]);
    match observed.user_flags {
        None => hasher.update(&[0]),
        Some(flags) => hasher.update(&[1]).update(&flags.to_le_bytes()),
    };
    match &observed.attributes {
        None => {
            hasher.update(&[0]);
        }
        Some(attributes) => {
            let tracked: Vec<_> = attributes
                .iter()
                .filter(|attribute| tracked_attribute(observed, attribute.name))
                .collect();
            hasher.update(&[1]);
            hasher.update(&(tracked.len() as u64).to_le_bytes());
            for attribute in tracked {
                hasher.update(&(attribute.name.len() as u64).to_le_bytes());
                hasher.update(attribute.name);
                hasher.update(&attribute.size.to_le_bytes());
                hasher.update(attribute.digest.as_bytes());
            }
        }
    }
    finish(&hasher)
}

fn update_access_control(hasher: &mut blake3::Hasher, access_control: Option<&AccessControl>) {
    match access_control {
        None | Some(AccessControl::NotReported) => hasher.update(&[0]),
        Some(AccessControl::MacOs(None)) => hasher.update(&[1]),
        Some(AccessControl::MacOs(Some(acl))) => hasher
            .update(&[2])
            .update(&(acl.len() as u64).to_le_bytes())
            .update(acl),
    };
}

fn tracked_attribute(observed: &Observed<'_>, name: &[u8]) -> bool {
    if SYSTEM_MANAGED.contains(&name) {
        return false;
    }
    if observed.compressed && (name == COMPRESSION || name == RESOURCE_FORK) {
        return false;
    }
    // A file's fork is content; elsewhere it is ordinary metadata.
    !(observed.kind == NodeKind::File && name == RESOURCE_FORK)
}

/// Seconds and nanoseconds with an explicit sign, so pre-1970 times stay exact.
fn update_time(hasher: &mut blake3::Hasher, time: SystemTime) {
    let (sign, duration) = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (1, duration),
        Err(before) => (0, before.duration()),
    };
    hasher.update(&[sign]);
    hasher.update(&duration.as_secs().to_le_bytes());
    hasher.update(&duration.subsec_nanos().to_le_bytes());
}

fn finish(hasher: &blake3::Hasher) -> Digest {
    Digest::from_bytes(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const ALL: MetadataPolicy =
        MetadataPolicy::new(Propagation::Sync, Propagation::Sync, Propagation::Sync);

    fn digest(byte: u8) -> Digest {
        Digest::from_bytes([byte; Digest::BYTE_LEN])
    }

    fn attribute(name: &[u8], byte: u8) -> Attribute<'_> {
        Attribute {
            name,
            size: 1,
            digest: digest(byte),
        }
    }

    fn file(attributes: Vec<Attribute<'_>>) -> Observed<'_> {
        Observed {
            kind: NodeKind::File,
            permissions: 0o100_644,
            modified: UNIX_EPOCH + Duration::from_secs(1_000),
            user_flags: Some(0),
            compressed: false,
            attributes: Some(attributes),
            access_control: None,
        }
    }

    fn version(observed: &Observed<'_>) -> EntryVersion {
        entry_version(Payload::File(digest(1)), observed, ALL)
    }

    #[test]
    fn resource_forks_are_content_but_other_xattrs_are_metadata() {
        let plain = version(&file(vec![]));
        let fork = version(&file(vec![attribute(RESOURCE_FORK, 2)]));
        let edited_fork = version(&file(vec![attribute(RESOURCE_FORK, 3)]));
        let tagged = version(&file(vec![attribute(
            b"com.apple.metadata:_kMDItemUserTags",
            2,
        )]));

        assert_ne!(plain.content(), fork.content());
        assert_ne!(fork.content(), edited_fork.content());
        assert_eq!(fork.metadata(), plain.metadata());
        assert_eq!(fork.metadata(), edited_fork.metadata());
        assert_eq!(tagged.content(), plain.content());
        assert_ne!(tagged.metadata(), plain.metadata());
    }

    #[test]
    fn compression_storage_and_system_attributes_do_not_change_versions() {
        let plain = version(&file(vec![]));
        let mut compressed = file(vec![attribute(COMPRESSION, 2), attribute(RESOURCE_FORK, 3)]);
        compressed.compressed = true;
        assert_eq!(version(&compressed), plain);

        for name in SYSTEM_MANAGED {
            assert_eq!(version(&file(vec![attribute(name, 4)])), plain);
        }
    }

    #[test]
    fn file_metadata_covers_mode_mtime_flags_and_acl_but_not_file_type_bits() {
        let base = file(vec![]);
        let plain = version(&base);

        let mut same_mode = file(vec![]);
        same_mode.permissions = 0o644;
        assert_eq!(version(&same_mode), plain);

        let mut mode = file(vec![]);
        mode.permissions = 0o100_600;
        let mut touched = file(vec![]);
        touched.modified = base.modified + Duration::from_nanos(1);
        let mut hidden = file(vec![]);
        hidden.user_flags = Some(0x8000);
        let mut unreported = file(vec![]);
        unreported.user_flags = None;
        let acl = AccessControl::MacOs(Some(vec![1, 2, 3]));
        let mut controlled = file(vec![]);
        controlled.access_control = Some(&acl);
        let no_acl = AccessControl::MacOs(None);
        let mut explicit_none = file(vec![]);
        explicit_none.access_control = Some(&no_acl);

        for changed in [mode, touched, hidden, unreported, controlled, explicit_none] {
            let changed = version(&changed);
            assert_eq!(changed.content(), plain.content());
            assert_ne!(changed.metadata(), plain.metadata());
        }
    }

    #[test]
    fn times_before_the_epoch_are_distinct_from_their_mirror_image() {
        let mut before = file(vec![]);
        before.modified = UNIX_EPOCH - Duration::from_secs(1_000);
        assert_ne!(version(&before), version(&file(vec![])));
    }

    #[test]
    fn directory_and_link_versions_ignore_their_mtimes() {
        let mut directory = file(vec![]);
        directory.kind = NodeKind::Directory;
        let first = entry_version(Payload::Directory, &directory, ALL);
        directory.modified += Duration::from_secs(1);
        assert_eq!(entry_version(Payload::Directory, &directory, ALL), first);
        assert_eq!(first.content(), EntryContent::Directory);

        let mut link = file(vec![]);
        link.kind = NodeKind::Symlink;
        link.attributes = None;
        let target = entry_version(Payload::Symlink(b"a"), &link, ALL);
        link.modified += Duration::from_secs(1);
        link.permissions = 0o777;
        assert_eq!(entry_version(Payload::Symlink(b"a"), &link, ALL), target);
        assert_ne!(entry_version(Payload::Symlink(b"b"), &link, ALL), target);
    }

    #[test]
    fn link_targets_and_file_data_never_share_digests() {
        let data = blake3::hash(b"a");
        let file = entry_version(
            Payload::File(Digest::from_bytes(*data.as_bytes())),
            &file(vec![]),
            ALL,
        );
        let mut link = super::tests::file(vec![]);
        link.kind = NodeKind::Symlink;
        let link = entry_version(Payload::Symlink(b"a"), &link, ALL);
        let (EntryContent::File(file), EntryContent::Symlink(link)) =
            (file.content(), link.content())
        else {
            panic!("unexpected content kinds");
        };
        assert_ne!(file, link);
        assert_ne!(file.as_bytes(), data.as_bytes());
    }

    fn with(observed: &Observed<'_>, policy: MetadataPolicy) -> EntryVersion {
        entry_version(Payload::File(digest(1)), observed, policy)
    }

    #[test]
    fn default_policy_syncs_times_and_attributes_but_only_the_executable_bit() {
        let policy = MetadataPolicy::default();
        let plain = with(&file(vec![]), policy);

        let mut mode = file(vec![]);
        mode.permissions = 0o100_600;
        let acl = AccessControl::MacOs(Some(vec![1]));
        let mut controlled = file(vec![]);
        controlled.access_control = Some(&acl);
        assert_eq!(with(&mode, policy), plain);
        assert_eq!(with(&controlled, policy), plain);

        let mut executable = file(vec![]);
        executable.permissions = 0o100_744;
        let mut touched = file(vec![]);
        touched.modified += Duration::from_secs(1);
        let tagged = file(vec![attribute(b"com.apple.metadata:_kMDItemUserTags", 2)]);
        let mut hidden = file(vec![]);
        hidden.user_flags = Some(0x8000);
        for changed in [executable, touched, tagged, hidden] {
            assert_ne!(with(&changed, policy).metadata(), plain.metadata());
        }
    }

    #[test]
    fn ignored_categories_never_change_versions_but_forks_stay_content() {
        let nothing = MetadataPolicy::new(
            Propagation::Ignore,
            Propagation::Ignore,
            Propagation::Ignore,
        );
        let plain = with(&file(vec![]), nothing);

        let mut touched = file(vec![]);
        touched.modified += Duration::from_secs(1);
        let tagged = file(vec![attribute(b"com.apple.metadata:_kMDItemUserTags", 2)]);
        let mut hidden = file(vec![]);
        hidden.user_flags = Some(0x8000);
        for unchanged in [touched, tagged, hidden] {
            assert_eq!(with(&unchanged, nothing), plain);
        }

        let forked = with(&file(vec![attribute(RESOURCE_FORK, 2)]), nothing);
        assert_ne!(forked.content(), plain.content());
        assert_eq!(forked.metadata(), plain.metadata());
    }

    #[test]
    fn digests_from_different_policies_never_match() {
        let observed = file(vec![]);
        let policies = [
            ALL,
            MetadataPolicy::default(),
            MetadataPolicy::new(
                Propagation::Ignore,
                Propagation::Ignore,
                Propagation::Ignore,
            ),
        ];
        for (index, left) in policies.iter().enumerate() {
            for right in &policies[index + 1..] {
                assert_ne!(
                    with(&observed, *left).metadata(),
                    with(&observed, *right).metadata()
                );
            }
        }
    }
}
