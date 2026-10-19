use std::io;
use std::mem::size_of;
use std::os::fd::{AsFd, AsRawFd};

use uuid::Uuid;

mod features;
pub(super) use features::filesystem_features;

#[repr(C)]
struct DeviceReply {
    length: u32,
    returned: libc::attribute_set_t,
    device: libc::dev_t,
}

#[repr(C)]
struct UuidReply {
    length: u32,
    returned: libc::attribute_set_t,
    uuid: uuid::Bytes,
}

const PREFIX_BYTES: usize = size_of::<u32>() + size_of::<libc::attribute_set_t>();

fn empty_set() -> libc::attribute_set_t {
    libc::attribute_set_t {
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

fn request(common: u32, volume: u32) -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | common,
        volattr: volume,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

fn valid_set(returned: &libc::attribute_set_t, common: u32, volume: u32) -> bool {
    returned.commonattr & libc::ATTR_CMN_RETURNED_ATTRS != 0
        && returned.commonattr & !(libc::ATTR_CMN_RETURNED_ATTRS | common) == 0
        && returned.volattr & !volume == 0
        && returned.dirattr == 0
        && returned.fileattr == 0
        && returned.forkattr == 0
}

fn invalid_reply() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid volume attribute reply")
}

pub(super) fn real_device(directory: &impl AsFd) -> io::Result<libc::dev_t> {
    let mut attributes = request(libc::ATTR_CMN_DEVID, 0);
    let mut reply = DeviceReply {
        length: 0,
        returned: empty_set(),
        device: 0,
    };
    // SAFETY: the borrowed descriptor remains live. Both buffers have C layout
    // and 4-byte alignment. Every byte is initialized and all field bit patterns
    // are valid; the fixed request returns only the length, attribute set and
    // dev_t. The synchronous call cannot retain either pointer.
    let result = unsafe {
        libc::fgetattrlist(
            directory.as_fd().as_raw_fd(),
            std::ptr::from_mut(&mut attributes).cast(),
            std::ptr::from_mut(&mut reply).cast(),
            size_of::<DeviceReply>(),
            libc::FSOPT_REPORT_FULLSIZE | libc::FSOPT_RETURN_REALDEV,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if reply.length as usize != size_of::<DeviceReply>()
        || !valid_set(&reply.returned, libc::ATTR_CMN_DEVID, 0)
        || reply.returned.commonattr & libc::ATTR_CMN_DEVID == 0
    {
        return Err(invalid_reply());
    }
    Ok(reply.device)
}

pub(super) fn uuid(directory: &impl AsFd) -> io::Result<Option<Uuid>> {
    let volume = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
    let mut attributes = request(0, volume);
    let mut reply = UuidReply {
        length: 0,
        returned: empty_set(),
        uuid: [0; size_of::<uuid::Bytes>()],
    };
    // SAFETY: the live descriptor and initialized C-layout buffers are valid for
    // this fixed length/attribute-set/UUID response. UUID bytes permit every bit
    // pattern. The kernel receives the exact capacity and retains no pointers.
    let result = unsafe {
        libc::fgetattrlist(
            directory.as_fd().as_raw_fd(),
            std::ptr::from_mut(&mut attributes).cast(),
            std::ptr::from_mut(&mut reply).cast(),
            size_of::<UuidReply>(),
            libc::FSOPT_REPORT_FULLSIZE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    decode_uuid(reply)
}

fn decode_uuid(reply: UuidReply) -> io::Result<Option<Uuid>> {
    let length = reply.length as usize;
    if !(PREFIX_BYTES..=size_of::<UuidReply>()).contains(&length)
        || !valid_set(
            &reply.returned,
            0,
            libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        )
    {
        return Err(invalid_reply());
    }
    if reply.returned.volattr & libc::ATTR_VOL_UUID == 0 {
        return Ok(None);
    }
    if length != size_of::<UuidReply>() {
        return Err(invalid_reply());
    }
    let uuid = Uuid::from_bytes(reply.uuid);
    Ok((!uuid.is_nil()).then_some(uuid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Root;

    #[test]
    fn malformed_or_unreported_uuid_attributes_never_create_identity() {
        for length in [0, PREFIX_BYTES - 1, size_of::<UuidReply>() + 1] {
            let mut returned = empty_set();
            returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
            returned.volattr = libc::ATTR_VOL_UUID;
            let reply = UuidReply {
                length: length as u32,
                returned,
                uuid: [1; size_of::<uuid::Bytes>()],
            };
            assert_eq!(
                decode_uuid(reply).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        let mut reply = UuidReply {
            length: PREFIX_BYTES as u32,
            returned: empty_set(),
            uuid: [1; size_of::<uuid::Bytes>()],
        };
        reply.returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
        assert_eq!(decode_uuid(reply).unwrap(), None);
        let mut reply = UuidReply {
            length: PREFIX_BYTES as u32,
            returned: empty_set(),
            uuid: [1; size_of::<uuid::Bytes>()],
        };
        reply.returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
        reply.returned.volattr = libc::ATTR_VOL_UUID;
        assert!(decode_uuid(reply).is_err());

        let mut returned = empty_set();
        returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
        returned.volattr = libc::ATTR_VOL_UUID;
        let reply = UuidReply {
            length: size_of::<UuidReply>() as u32,
            returned,
            uuid: [1; size_of::<uuid::Bytes>()],
        };
        assert_eq!(
            decode_uuid(reply).unwrap(),
            Some(Uuid::from_bytes([1; size_of::<uuid::Bytes>()]))
        );
        let mut returned = empty_set();
        returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
        returned.volattr = libc::ATTR_VOL_UUID;
        let reply = UuidReply {
            length: size_of::<UuidReply>() as u32,
            returned,
            uuid: [0; size_of::<uuid::Bytes>()],
        };
        assert_eq!(decode_uuid(reply).unwrap(), None);
        let mut returned = empty_set();
        returned.commonattr = u32::MAX;
        returned.volattr = libc::ATTR_VOL_UUID;
        let reply = UuidReply {
            length: size_of::<UuidReply>() as u32,
            returned,
            uuid: [1; size_of::<uuid::Bytes>()],
        };
        assert!(decode_uuid(reply).is_err());
    }

    #[test]
    fn held_root_uuid_matches_foundation_and_survives_ambient_moves() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let root = Root::open(&selected).unwrap();
        let observed = root
            .volume_uuid()
            .unwrap()
            .expect("native fixture volume reports a UUID");
        let output = std::process::Command::new("/usr/bin/xcrun")
            .args(["swift", "-e", "import Foundation; let url = URL(fileURLWithPath: ProcessInfo.processInfo.environment[\"FOLDOVER_VOLUME_TEST_ROOT\"]!); print(try url.resourceValues(forKeys: [.volumeUUIDStringKey]).volumeUUIDString ?? \"\")"])
            .env("FOLDOVER_VOLUME_TEST_ROOT", &selected).stdin(std::process::Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reference = Uuid::parse_str(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
        assert_eq!(observed, reference);
        std::fs::rename(&selected, temporary.path().join("moved")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert_eq!(root.volume_uuid().unwrap(), Some(observed));
    }
}
