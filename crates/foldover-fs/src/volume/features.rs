use super::{PREFIX_BYTES, empty_set, invalid_reply, request, valid_set};
use crate::features::FEATURE_COUNT;
use crate::{DeclaredSupport, FilesystemFeature, FilesystemFeatures};
use std::io;
use std::mem::size_of;
use std::os::fd::{AsFd, AsRawFd};

#[repr(C)]
struct FeatureReply {
    length: u32,
    returned: libc::attribute_set_t,
    features: libc::vol_capabilities_attr_t,
}

// The array word is part of the SDK contract; equal bits in different words
// describe unrelated features. Keep every mapping beside its named constant.
const FEATURE_BITS: [(FilesystemFeature, usize, u32); FEATURE_COUNT] = [
    (
        FilesystemFeature::CaseSensitiveNames,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_CASE_SENSITIVE,
    ),
    (
        FilesystemFeature::CasePreservedNames,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_CASE_PRESERVING,
    ),
    (
        FilesystemFeature::SymbolicLinks,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_SYMBOLICLINKS,
    ),
    (
        FilesystemFeature::HardLinks,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_HARDLINKS,
    ),
    (
        FilesystemFeature::SparseFiles,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_SPARSE_FILES,
    ),
    (
        FilesystemFeature::PersistentObjectIds,
        libc::VOL_CAPABILITIES_FORMAT,
        libc::VOL_CAP_FMT_PERSISTENTOBJECTIDS,
    ),
    (
        FilesystemFeature::NativeXattrs,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_EXTENDED_ATTR,
    ),
    (
        FilesystemFeature::AccessControlLists,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_EXTENDED_SECURITY,
    ),
    (
        FilesystemFeature::NamedStreams,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_NAMEDSTREAMS,
    ),
    (
        FilesystemFeature::FileCloning,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_CLONE,
    ),
    (
        FilesystemFeature::ExclusiveRename,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_RENAME_EXCL,
    ),
    (
        FilesystemFeature::SwapRename,
        libc::VOL_CAPABILITIES_INTERFACES,
        libc::VOL_CAP_INT_RENAME_SWAP,
    ),
];

pub(crate) fn filesystem_features(directory: &impl AsFd) -> io::Result<FilesystemFeatures> {
    let mut attributes = request(0, libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES);
    let mut reply = FeatureReply {
        length: 0,
        returned: empty_set(),
        features: libc::vol_capabilities_attr_t {
            capabilities: Default::default(),
            valid: Default::default(),
        },
    };
    // SAFETY: the descriptor is borrowed for this synchronous call. Both buffers
    // have C layout with 4-byte-aligned integer fields and are fully initialized.
    // The fixed request has no pointers/variable fields in its response. The
    // exact capacity is supplied and REPORT_FULLSIZE detects truncation.
    let result = unsafe {
        libc::fgetattrlist(
            directory.as_fd().as_raw_fd(),
            std::ptr::from_mut(&mut attributes).cast(),
            std::ptr::from_mut(&mut reply).cast(),
            size_of::<FeatureReply>(),
            libc::FSOPT_REPORT_FULLSIZE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    decode(reply)
}

fn decode(reply: FeatureReply) -> io::Result<FilesystemFeatures> {
    if !valid_set(
        &reply.returned,
        0,
        libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
    ) {
        return Err(invalid_reply());
    }
    if reply.returned.volattr & libc::ATTR_VOL_CAPABILITIES == 0 {
        if reply.length as usize != PREFIX_BYTES {
            return Err(invalid_reply());
        }
        return Ok(FilesystemFeatures::default());
    }
    if reply.length as usize != size_of::<FeatureReply>() {
        return Err(invalid_reply());
    }
    Ok(FilesystemFeatures::from_reports(FEATURE_BITS.map(
        |(feature, word, bit)| {
            let support = if reply.features.valid[word] & bit == 0 {
                DeclaredSupport::NotReported
            } else if reply.features.capabilities[word] & bit == 0 {
                DeclaredSupport::Unsupported
            } else {
                DeclaredSupport::Supported
            };
            (feature, support)
        },
    )))
}

#[cfg(test)]
mod tests;
