//! Pairs whose folders changed, for change-triggered runs.

use std::io;

use foldover_endpoint::RootWatcher;
use foldover_types::{PairConfig, PairId};

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

    /// Watch both folders of exactly these pairs. Folders that cannot be
    /// watched, such as unmounted disks, are returned with their pair.
    pub fn watch(&mut self, pairs: &[PairConfig]) -> Vec<(PairId, io::Error)> {
        let roots: Vec<_> = pairs
            .iter()
            .flat_map(|pair| {
                let draft = pair.draft();
                [
                    (draft.left().clone(), pair.id()),
                    (draft.right().clone(), pair.id()),
                ]
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
