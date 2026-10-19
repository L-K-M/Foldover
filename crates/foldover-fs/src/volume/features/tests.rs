use super::*;
use crate::Root;
use std::fs;

fn reported() -> FeatureReply {
    let mut returned = empty_set();
    returned.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
    returned.volattr = libc::ATTR_VOL_CAPABILITIES;
    FeatureReply {
        length: size_of::<FeatureReply>() as u32,
        returned,
        features: libc::vol_capabilities_attr_t {
            capabilities: Default::default(),
            valid: Default::default(),
        },
    }
}

#[test]
fn every_feature_requires_its_own_valid_bit_and_array_word() {
    for (feature, word, bit) in FEATURE_BITS {
        for (capability, valid, expected) in [
            (0, 0, DeclaredSupport::NotReported),
            (bit, 0, DeclaredSupport::NotReported),
            (0, bit, DeclaredSupport::Unsupported),
            (bit, bit, DeclaredSupport::Supported),
        ] {
            let mut reply = reported();
            reply.features.capabilities[word] = capability;
            reply.features.valid[word] = valid;
            let result = decode(reply).unwrap();
            assert_eq!(result.support(feature), expected, "{feature:?}");
            for (other, _, _) in FEATURE_BITS {
                if other != feature {
                    assert_eq!(result.support(other), DeclaredSupport::NotReported);
                }
            }
        }
        let mut wrong_word = reported();
        let other_word = if word == libc::VOL_CAPABILITIES_FORMAT {
            libc::VOL_CAPABILITIES_INTERFACES
        } else {
            libc::VOL_CAPABILITIES_FORMAT
        };
        wrong_word.features.capabilities[other_word] = bit;
        wrong_word.features.valid[other_word] = bit;
        assert_eq!(
            decode(wrong_word).unwrap().support(feature),
            DeclaredSupport::NotReported
        );
    }
}

#[test]
fn absent_and_malformed_attribute_replies_never_manufacture_support() {
    let mut absent = reported();
    absent.length = PREFIX_BYTES as u32;
    absent.returned.volattr = 0;
    absent.features.capabilities.fill(u32::MAX);
    absent.features.valid.fill(u32::MAX);
    assert_eq!(decode(absent).unwrap(), FilesystemFeatures::default());

    for length in [
        0,
        PREFIX_BYTES - 1,
        PREFIX_BYTES,
        size_of::<FeatureReply>() - 1,
        size_of::<FeatureReply>() + 1,
    ] {
        let mut reply = reported();
        reply.length = length as u32;
        assert_eq!(
            decode(reply).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    let mut absent_with_payload = reported();
    absent_with_payload.returned.volattr = 0;
    assert!(decode(absent_with_payload).is_err());
    for unexpected in [libc::ATTR_CMN_NAME, libc::ATTR_CMN_DEVID] {
        let mut reply = reported();
        reply.returned.commonattr |= unexpected;
        assert!(decode(reply).is_err());
    }
    let mut no_returned_mask = reported();
    no_returned_mask.returned.commonattr = 0;
    assert!(decode(no_returned_mask).is_err());
    let mut unrelated_volume = reported();
    unrelated_volume.returned.volattr |= libc::ATTR_VOL_UUID;
    assert!(decode(unrelated_volume).is_err());
    for group in 0..3 {
        let mut reply = reported();
        match group {
            0 => reply.returned.dirattr = u32::MAX,
            1 => reply.returned.fileattr = u32::MAX,
            _ => reply.returned.forkattr = u32::MAX,
        }
        assert!(decode(reply).is_err());
    }
}

#[test]
fn binding_layout_and_feature_constants_match_the_native_sdk() {
    let source = r#"
        import Darwin
        print(MemoryLayout<vol_capabilities_attr_t>.size, MemoryLayout<vol_capabilities_attr_t>.alignment)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_CASE_SENSITIVE)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_CASE_PRESERVING)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_SYMBOLICLINKS)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_HARDLINKS)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_SPARSE_FILES)
        print(VOL_CAPABILITIES_FORMAT, VOL_CAP_FMT_PERSISTENTOBJECTIDS)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_EXTENDED_ATTR)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_EXTENDED_SECURITY)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_NAMEDSTREAMS)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_CLONE)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_RENAME_EXCL)
        print(VOL_CAPABILITIES_INTERFACES, VOL_CAP_INT_RENAME_SWAP)
    "#;
    let output = std::process::Command::new("/usr/bin/xcrun")
        .args(["swift", "-e", source])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let mut expected = vec![
        size_of::<libc::vol_capabilities_attr_t>() as u64,
        std::mem::align_of::<libc::vol_capabilities_attr_t>() as u64,
    ];
    expected.extend(
        FEATURE_BITS
            .into_iter()
            .flat_map(|(_, word, bit)| [word as u64, u64::from(bit)]),
    );
    assert_eq!(observed, expected);
}

#[test]
fn held_volume_declarations_match_foundation_and_survive_ambient_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = temporary.path().join("selected");
    fs::create_dir(&selected).unwrap();
    let root = Root::open(&selected).unwrap();
    let observed = root.filesystem_features().unwrap();
    let source = r#"
        import Foundation
        let url = URL(fileURLWithPath: ProcessInfo.processInfo.environment["FOLDOVER_FEATURE_TEST_ROOT"]!)
        let values = try url.resourceValues(forKeys: [
            .volumeSupportsCaseSensitiveNamesKey, .volumeSupportsCasePreservedNamesKey,
            .volumeSupportsSymbolicLinksKey, .volumeSupportsHardLinksKey,
            .volumeSupportsSparseFilesKey, .volumeSupportsPersistentIDsKey,
            .volumeSupportsExtendedSecurityKey, .volumeSupportsFileCloningKey
        ])
        for value in [values.volumeSupportsCaseSensitiveNames, values.volumeSupportsCasePreservedNames,
                      values.volumeSupportsSymbolicLinks, values.volumeSupportsHardLinks,
                      values.volumeSupportsSparseFiles, values.volumeSupportsPersistentIDs,
                      values.volumeSupportsExtendedSecurity, values.volumeSupportsFileCloning] {
            print(value.map { $0 ? "supported" : "unsupported" } ?? "not-reported")
        }
    "#;
    let output = std::process::Command::new("/usr/bin/xcrun")
        .args(["swift", "-e", source])
        .env("FOLDOVER_FEATURE_TEST_ROOT", &selected)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reference = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .map(|value| match value {
            "supported" => DeclaredSupport::Supported,
            "unsupported" => DeclaredSupport::Unsupported,
            "not-reported" => DeclaredSupport::NotReported,
            _ => panic!("Unexpected Foundation declaration: {value}"),
        })
        .collect::<Vec<_>>();
    let features = [
        FilesystemFeature::CaseSensitiveNames,
        FilesystemFeature::CasePreservedNames,
        FilesystemFeature::SymbolicLinks,
        FilesystemFeature::HardLinks,
        FilesystemFeature::SparseFiles,
        FilesystemFeature::PersistentObjectIds,
        FilesystemFeature::AccessControlLists,
        FilesystemFeature::FileCloning,
    ];
    assert_eq!(
        features.map(|feature| observed.support(feature)).as_slice(),
        reference
    );
    fs::rename(&selected, temporary.path().join("moved")).unwrap();
    std::os::unix::fs::symlink("/unavailable", &selected).unwrap();
    assert_eq!(root.filesystem_features().unwrap(), observed);
}

#[test]
fn native_case_declaration_matches_owned_directory_behavior() {
    let temporary = tempfile::tempdir().unwrap();
    let root = Root::open(temporary.path()).unwrap();
    let declared = root
        .filesystem_features()
        .unwrap()
        .support(FilesystemFeature::CaseSensitiveNames);
    fs::create_dir(temporary.path().join("MixedCase")).unwrap();
    let second = fs::create_dir(temporary.path().join("mixedcase"));
    match declared {
        DeclaredSupport::Supported => second.unwrap(),
        DeclaredSupport::Unsupported => {
            assert_eq!(second.unwrap_err().kind(), io::ErrorKind::AlreadyExists)
        }
        DeclaredSupport::NotReported => {
            panic!("The native fixture volume must declare case sensitivity")
        }
    }
}
