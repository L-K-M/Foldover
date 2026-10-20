//! Whether a pair's folders are present, so unattended runs wait for an
//! unmounted disk or share instead of failing; and what will not work
//! as a person might expect with the folders they picked, told to them
//! before they save (PLAN.md §3.6).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use foldover_endpoint::{
    CloudProvider, DeclaredSupport, Endpoint, EndpointPair, FilesystemFeature, PairRootError,
    Reach, current_cloud_provider, mount_share,
};
use foldover_types::{
    CaseMatching, NameMatching, PairConfig, PairDraft, PairRoot, Propagation, Side,
};

use crate::{Engine, Error};

/// Mount `side`'s share again from `source`, its recorded mount source;
/// true when the folder is back.
pub fn reconnect(pair: &PairConfig, side: Side, source: &str, reach: &Reach) -> bool {
    mount_share(source).is_ok() && missing_side(pair, reach) != Some(side)
}

/// The side whose folder is missing, if either is.
pub fn missing_side(pair: &PairConfig, reach: &Reach) -> Option<Side> {
    match EndpointPair::open(pair.draft().left(), pair.draft().right(), reach) {
        // An unreachable server is waited for like an unmounted disk.
        Err(PairRootError::Missing(side) | PairRootError::Unreachable(side, _)) => Some(side),
        _ => None,
    }
}

/// What will not work as a person might expect with the folders they
/// picked (PLAN.md §3.6). A warning never blocks saving.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadinessWarning {
    /// Another sync engine manages this side's folder.
    CloudManaged { side: Side, provider: CloudProvider },
    /// The pair syncs extended attributes this side cannot keep.
    MetadataUnsupported { side: Side },
    /// One side ignores name case and the other does not; this side
    /// ignores it.
    NamesDiffer { side: Side },
}

impl Engine {
    /// What will not work as a person might expect with `draft`'s
    /// folders. A root that cannot be opened yields no warnings for its
    /// side: saving reports that itself.
    pub fn check_pair(&self, draft: &PairDraft) -> Result<Vec<ReadinessWarning>, Error> {
        self.check_pair_with(draft, current_cloud_provider)
    }

    /// `check_pair` with the home-directory lookup injectable for tests.
    fn check_pair_with(
        &self,
        draft: &PairDraft,
        cloud: impl Fn(&Path) -> Option<CloudProvider>,
    ) -> Result<Vec<ReadinessWarning>, Error> {
        let reach = self.reach();
        let mut warnings = Vec::new();
        let mut matching = [None, None];
        for side in [Side::Left, Side::Right] {
            let root = match side {
                Side::Left => draft.left(),
                Side::Right => draft.right(),
            };
            // Open like pair creation does; a root that will not open is
            // reported by saving instead, so it earns no warnings here.
            let Ok(endpoint) = Endpoint::open(root, &reach) else {
                continue;
            };
            if let PairRoot::Local(local) = root {
                let path = Path::new(OsStr::from_bytes(local.as_bytes()));
                if let Some(provider) = cloud(path) {
                    warnings.push(ReadinessWarning::CloudManaged { side, provider });
                }
            }
            let syncs = draft.options().metadata().extended_attributes() == Propagation::Sync;
            if syncs && !keeps_extended_attributes(&endpoint)? {
                warnings.push(ReadinessWarning::MetadataUnsupported { side });
            }
            matching[side as usize] = Some(endpoint.name_matching());
        }
        if let [Some(left), Some(right)] = matching
            && let Some(side) = case_differs(left, right)
        {
            warnings.push(ReadinessWarning::NamesDiffer { side });
        }
        Ok(warnings)
    }
}

/// Whether the endpoint can keep extended attributes. Another Mac can
/// and SFTP never can; a local filesystem can unless it declares them
/// unsupported — a filesystem that does not report them earns no
/// warning. Simulated folders keep none.
fn keeps_extended_attributes(endpoint: &Endpoint) -> Result<bool, Error> {
    if let Endpoint::Local(local) = endpoint {
        return Ok(local
            .filesystem_features()?
            .support(FilesystemFeature::NativeXattrs)
            != DeclaredSupport::Unsupported);
    }
    Ok(matches!(endpoint, Endpoint::Peer(_)))
}

/// The side ignoring name case, when one side does and the other does
/// not: names differing only in case become conflicts to rename.
fn case_differs(left: NameMatching, right: NameMatching) -> Option<Side> {
    if left.case() == right.case() {
        return None;
    }
    Some(if left.case() == CaseMatching::Insensitive {
        Side::Left
    } else {
        Side::Right
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_endpoint::cloud_provider;
    use foldover_types::{LocalRoot, SyncMode, UnicodeMatching};
    use std::fs;

    fn root(path: &Path) -> LocalRoot {
        LocalRoot::from_bytes(path.as_os_str().as_bytes().to_vec()).unwrap()
    }

    fn engine() -> (tempfile::TempDir, Engine) {
        let temporary = tempfile::tempdir().unwrap();
        let location = foldover_state::StateLocation::Directory(temporary.path().join("state"));
        (temporary, Engine::open(&location).unwrap())
    }

    #[test]
    fn a_cloud_managed_root_names_its_side_and_provider() {
        let (temporary, engine) = engine();
        let home = tempfile::tempdir().unwrap();
        let cloud = home.path().join("Library/Mobile Documents");
        fs::create_dir_all(&cloud).unwrap();
        let left = cloud.join("Photos");
        let right = temporary.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let draft =
            PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync).unwrap();
        let warnings = engine
            .check_pair_with(&draft, |path| cloud_provider(home.path(), path))
            .unwrap();
        assert_eq!(
            warnings,
            [ReadinessWarning::CloudManaged {
                side: Side::Left,
                provider: CloudProvider::ICloudDrive,
            }]
        );
    }

    #[test]
    fn a_root_that_will_not_open_yields_no_warnings_for_its_side() {
        let (temporary, engine) = engine();
        let present = temporary.path().join("present");
        fs::create_dir(&present).unwrap();
        let missing = temporary.path().join("missing");
        for (left, right) in [(&missing, &present), (&present, &missing)] {
            let draft =
                PairDraft::new("Pair".into(), root(left), root(right), SyncMode::Sync).unwrap();
            assert_eq!(engine.check_pair(&draft).unwrap(), []);
        }
    }

    #[test]
    fn ordinary_folders_warn_about_nothing() {
        let (temporary, engine) = engine();
        let left = temporary.path().join("left");
        let right = temporary.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let draft =
            PairDraft::new("Pair".into(), root(&left), root(&right), SyncMode::Sync).unwrap();
        assert_eq!(engine.check_pair(&draft).unwrap(), []);
    }

    #[test]
    fn name_case_differs_exactly_when_one_side_ignores_it() {
        let insensitive = NameMatching::new(CaseMatching::Insensitive, UnicodeMatching::Normalized);
        let exact = NameMatching::EXACT;
        assert_eq!(case_differs(insensitive, exact), Some(Side::Left));
        assert_eq!(case_differs(exact, insensitive), Some(Side::Right));
        assert_eq!(case_differs(insensitive, insensitive), None);
        assert_eq!(case_differs(exact, exact), None);
    }

    #[test]
    fn an_sftp_side_cannot_keep_extended_attributes() {
        let Some(server) = std::env::var_os("FOLDOVER_SFTP_SERVER")
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
        else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let endpoint = foldover_endpoint::SftpEndpoint::spawn(
            &server,
            &[OsStr::new("-d"), OsStr::new("/")],
            root.path().as_os_str().as_bytes(),
        )
        .unwrap();
        assert!(!keeps_extended_attributes(&Endpoint::Sftp(endpoint)).unwrap());
    }
}
