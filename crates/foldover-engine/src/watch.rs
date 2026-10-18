//! Pairs whose folders changed, for change-triggered runs.

use std::io;

use foldover_endpoint::RootWatcher;
use foldover_types::{PairConfig, PairId, PairRoot};

pub struct PairWatcher {
    roots: RootWatcher<PairId>,
}

impl PairWatcher {
    /// `changed` runs on the notification thread for each affected pair.
    pub fn new(changed: impl Fn(PairId) + Send + 'static) -> io::Result<Self> {
        Ok(Self {
            roots: RootWatcher::new(changed)?,
        })
    }

    /// Watch both local folders of exactly these pairs. Folders that cannot
    /// be watched, such as unmounted disks, are returned with their pair.
    /// SFTP folders send no notifications; their changes are found by the
    /// periodic verification run.
    pub fn watch(&mut self, pairs: &[PairConfig]) -> Vec<(PairId, io::Error)> {
        let roots: Vec<_> = pairs
            .iter()
            .flat_map(|pair| {
                let draft = pair.draft();
                [(draft.left(), pair.id()), (draft.right(), pair.id())]
            })
            .filter_map(|(root, id)| match root {
                PairRoot::Local(root) => Some((root.clone(), id)),
                PairRoot::Sftp(_) => None,
            })
            .collect();
        self.roots
            .set_roots(&roots)
            .into_iter()
            .filter_map(|(root, error)| {
                roots
                    .iter()
                    .find(|(candidate, _)| *candidate == root)
                    .map(|(_, id)| (*id, error))
            })
            .collect()
    }
}
