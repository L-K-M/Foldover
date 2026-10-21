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

use foldover_fs::{AccessControl, AttributeRole, NodeKind};
use foldover_types::{Digest, EntryContent, EntryVersion, MetadataPolicy, Propagation};

// Domain separation keeps content, link and metadata digests distinct from
// each other and from raw data hashes.
const FILE_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local file content v1";
const LINK_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local symlink target v1";
const METADATA_CONTEXT: &str = "ch.lkmc.foldover 2026-10-18 local entry metadata v1";

const PERMISSION_BITS: u32 = 0o7777;
const EXECUTE_BITS: u32 = 0o111;

pub(crate) enum Payload<'a> {
    File(Digest),
    Directory,
    Symlink(&'a [u8]),
}

#[derive(Clone, Copy)]
pub(crate) struct Attribute<'a> {
    pub(crate) role: AttributeRole,
    pub(crate) name: &'a [u8],
    pub(crate) size: u64,
    pub(crate) digest: Digest,
}

/// The observed fields a version depends on. `attributes` is `None` only where
/// the platform cannot report them (Linux symlinks), never after a failed read.
pub(crate) struct Observed<'a> {
    pub(crate) kind: NodeKind,
    pub(crate) permissions: u32,
    pub(crate) modified: SystemTime,
    pub(crate) user_flags: Option<u32>,
    pub(crate) attributes: Option<Vec<Attribute<'a>>>,
    pub(crate) access_control: Option<&'a AccessControl>,
}

pub(crate) fn entry_version(
    payload: Payload<'_>,
    observed: &Observed<'_>,
    policy: MetadataPolicy,
) -> EntryVersion {
    let attributes = observed.attributes.as_deref().unwrap_or_default();
    // The driver gives the resource-fork role only to uncompressed files.
    // Fork edits are content, so metadata-only conflict policy cannot drop them.
    let fork = attributes
        .iter()
        .find(|attribute| attribute.role == AttributeRole::ResourceFork);
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

/// A version whose content is already known, such as after a metadata-only
/// update, so the data need not be read again.
pub(crate) fn metadata_version(
    content: EntryContent,
    observed: &Observed<'_>,
    policy: MetadataPolicy,
) -> EntryVersion {
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
            let quarantine = policy.quarantine() == Propagation::Sync;
            let tracked: Vec<_> = attributes
                .iter()
                .filter(|attribute| match attribute.role {
                    AttributeRole::Synced => true,
                    AttributeRole::Quarantine => quarantine,
                    _ => false,
                })
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

    const FORK: &[u8] = b"com.apple.ResourceFork";
    const TAGS: &[u8] = b"com.apple.metadata:_kMDItemUserTags";

    fn attribute(role: AttributeRole, name: &[u8], byte: u8) -> Attribute<'_> {
        Attribute {
            role,
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
            attributes: Some(attributes),
            access_control: None,
        }
    }

    fn version(observed: &Observed<'_>) -> EntryVersion {
        entry_version(Payload::File(digest(1)), observed, ALL)
    }

    /// PLAN.md §4.10: Gatekeeper's mark counts only in runs it travels in,
    /// so a network volume without it never looks changed.
    #[test]
    fn quarantine_counts_only_where_it_travels() {
        const QUARANTINE: &[u8] = b"com.apple.quarantine";
        let marked = file(vec![attribute(AttributeRole::Quarantine, QUARANTINE, 7)]);
        let plain = file(Vec::new());
        let left_alone = ALL.with_quarantine(Propagation::Ignore);
        let at = |observed, policy| entry_version(Payload::File(digest(1)), observed, policy);
        assert_ne!(at(&marked, ALL), at(&plain, ALL));
        assert_eq!(at(&marked, left_alone), at(&plain, left_alone));
        let ignored =
            MetadataPolicy::new(Propagation::Sync, Propagation::Ignore, Propagation::Sync);
        assert_eq!(ignored.quarantine(), Propagation::Ignore);
        assert_eq!(ALL.quarantine(), Propagation::Sync);
    }

    #[test]
    fn resource_forks_are_content_but_other_xattrs_are_metadata() {
        let plain = version(&file(vec![]));
        let fork = version(&file(vec![attribute(AttributeRole::ResourceFork, FORK, 2)]));
        let edited_fork = version(&file(vec![attribute(AttributeRole::ResourceFork, FORK, 3)]));
        let tagged = version(&file(vec![attribute(AttributeRole::Synced, TAGS, 2)]));

        assert_ne!(plain.content(), fork.content());
        assert_ne!(fork.content(), edited_fork.content());
        assert_eq!(fork.metadata(), plain.metadata());
        assert_eq!(fork.metadata(), edited_fork.metadata());
        assert_eq!(tagged.content(), plain.content());
        assert_ne!(tagged.metadata(), plain.metadata());
    }

    #[test]
    fn only_synced_attributes_change_metadata() {
        let plain = version(&file(vec![]));
        let ignored = file(vec![
            attribute(AttributeRole::CompressionStorage, FORK, 3),
            attribute(AttributeRole::SystemManaged, b"com.apple.macl", 4),
        ]);
        assert_eq!(version(&ignored), plain);
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
        let tagged = file(vec![attribute(AttributeRole::Synced, TAGS, 2)]);
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
        let tagged = file(vec![attribute(AttributeRole::Synced, TAGS, 2)]);
        let mut hidden = file(vec![]);
        hidden.user_flags = Some(0x8000);
        for unchanged in [touched, tagged, hidden] {
            assert_eq!(with(&unchanged, nothing), plain);
        }

        let forked = with(
            &file(vec![attribute(AttributeRole::ResourceFork, FORK, 2)]),
            nothing,
        );
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
