//! FSEvents constants from <CoreServices/FSEvents.h>, kept portable so the
//! flag-name mapping is unit-testable on any platform.

// The complete kFSEventStreamCreateFlag set, kept as a reference table; the
// spike exercises only the subset in STREAM_CREATE_FLAGS.
#[allow(dead_code)]
pub mod create {
    pub const NONE: u32 = 0x0000_0000;
    pub const USE_CF_TYPES: u32 = 0x0000_0001;
    pub const NO_DEFER: u32 = 0x0000_0002;
    pub const WATCH_ROOT: u32 = 0x0000_0004;
    pub const IGNORE_SELF: u32 = 0x0000_0008;
    pub const FILE_EVENTS: u32 = 0x0000_0010;
    pub const MARK_SELF: u32 = 0x0000_0020;
    pub const USE_EXTENDED_DATA: u32 = 0x0000_0040;
    pub const FULL_HISTORY: u32 = 0x0000_0080;
}

pub mod event {
    pub const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
    pub const USER_DROPPED: u32 = 0x0000_0002;
    pub const KERNEL_DROPPED: u32 = 0x0000_0004;
    pub const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
    pub const HISTORY_DONE: u32 = 0x0000_0010;
    pub const ROOT_CHANGED: u32 = 0x0000_0020;
    pub const MOUNT: u32 = 0x0000_0040;
    pub const UNMOUNT: u32 = 0x0000_0080;
    pub const ITEM_CREATED: u32 = 0x0000_0100;
    pub const ITEM_REMOVED: u32 = 0x0000_0200;
    pub const ITEM_INODE_META_MOD: u32 = 0x0000_0400;
    pub const ITEM_RENAMED: u32 = 0x0000_0800;
    pub const ITEM_MODIFIED: u32 = 0x0000_1000;
    pub const ITEM_FINDER_INFO_MOD: u32 = 0x0000_2000;
    pub const ITEM_CHANGE_OWNER: u32 = 0x0000_4000;
    pub const ITEM_XATTR_MOD: u32 = 0x0000_8000;
    pub const ITEM_IS_FILE: u32 = 0x0001_0000;
    pub const ITEM_IS_DIR: u32 = 0x0002_0000;
    pub const ITEM_IS_SYMLINK: u32 = 0x0004_0000;
    pub const OWN_EVENT: u32 = 0x0008_0000;
    pub const ITEM_IS_HARDLINK: u32 = 0x0010_0000;
    pub const ITEM_IS_LAST_HARDLINK: u32 = 0x0020_0000;
    pub const ITEM_CLONED: u32 = 0x0040_0000;
}

/// The spike watches paths with these flags on every stream it opens.
pub const STREAM_CREATE_FLAGS: u32 = create::USE_CF_TYPES
    | create::FILE_EVENTS
    | create::WATCH_ROOT
    | create::NO_DEFER
    | create::USE_EXTENDED_DATA;

/// `kFSEventStreamEventIdSinceNow`: start a stream at the current event ID.
pub const SINCE_NOW: u64 = u64::MAX;

/// Flags that mean "the event stream could not see individual changes", any
/// of which forces a rescan of the watched tree.
pub const DROP_MASK: u32 = event::MUST_SCAN_SUBDIRS | event::USER_DROPPED | event::KERNEL_DROPPED;

/// Human-readable names for every known flag bit, for the report.
pub fn flag_names(flags: u32) -> Vec<String> {
    const NAMES: &[(u32, &str)] = &[
        (event::MUST_SCAN_SUBDIRS, "MustScanSubDirs"),
        (event::USER_DROPPED, "UserDropped"),
        (event::KERNEL_DROPPED, "KernelDropped"),
        (event::EVENT_IDS_WRAPPED, "EventIdsWrapped"),
        (event::HISTORY_DONE, "HistoryDone"),
        (event::ROOT_CHANGED, "RootChanged"),
        (event::MOUNT, "Mount"),
        (event::UNMOUNT, "Unmount"),
        (event::ITEM_CREATED, "ItemCreated"),
        (event::ITEM_REMOVED, "ItemRemoved"),
        (event::ITEM_INODE_META_MOD, "ItemInodeMetaMod"),
        (event::ITEM_RENAMED, "ItemRenamed"),
        (event::ITEM_MODIFIED, "ItemModified"),
        (event::ITEM_FINDER_INFO_MOD, "ItemFinderInfoMod"),
        (event::ITEM_CHANGE_OWNER, "ItemChangeOwner"),
        (event::ITEM_XATTR_MOD, "ItemXattrMod"),
        (event::ITEM_IS_FILE, "ItemIsFile"),
        (event::ITEM_IS_DIR, "ItemIsDir"),
        (event::ITEM_IS_SYMLINK, "ItemIsSymlink"),
        (event::OWN_EVENT, "OwnEvent"),
        (event::ITEM_IS_HARDLINK, "ItemIsHardlink"),
        (event::ITEM_IS_LAST_HARDLINK, "ItemIsLastHardlink"),
        (event::ITEM_CLONED, "ItemCloned"),
    ];
    let mut names: Vec<String> = NAMES
        .iter()
        .filter(|(bit, _)| (flags & bit) != 0)
        .map(|(_, name)| name.to_string())
        .collect();
    let known: u32 = NAMES.iter().fold(0, |a, (bit, _)| a | *bit);
    let unknown = flags & !known;
    if unknown != 0 {
        names.push(format!("0x{unknown:08x}"));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_map_single_and_combined_bits() {
        assert_eq!(flag_names(0), Vec::<String>::new());
        assert_eq!(flag_names(event::ITEM_CREATED), ["ItemCreated"]);
        assert_eq!(
            flag_names(event::ITEM_IS_FILE | event::ITEM_RENAMED),
            ["ItemRenamed", "ItemIsFile"]
        );
    }

    #[test]
    fn unknown_bits_surface_as_hex() {
        assert_eq!(flag_names(0x8000_0000), ["0x80000000"]);
    }
}
