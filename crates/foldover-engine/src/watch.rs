//! Pairs whose folders changed, for change-triggered runs.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use foldover_endpoint::{Reach, RootWatcher, WatchHandle, watch_folder};
use foldover_types::{PairConfig, PairId, PairRoot, PeerRoot};

type Changed = Arc<dyn Fn(PairId) + Send + Sync>;

pub struct PairWatcher {
    roots: RootWatcher<PairId>,
    changed: Changed,
    /// The pairs of each folder on another Mac, read as its reports come.
    peer_pairs: Arc<Mutex<HashMap<PeerRoot, Vec<PairId>>>>,
    /// Requests that keep paired Macs reporting.
    peers: HashMap<PeerRoot, WatchHandle>,
}

impl PairWatcher {
    /// `changed` runs on a notification thread for each affected pair.
    pub fn new(changed: impl Fn(PairId) + Send + Sync + 'static) -> io::Result<Self> {
        let changed: Changed = Arc::new(changed);
        let local = Arc::clone(&changed);
        Ok(Self {
            roots: RootWatcher::new(move |id| local(id))?,
            changed,
            peer_pairs: Arc::default(),
            peers: HashMap::new(),
        })
    }

    /// Watch both folders of exactly these pairs: local ones through
    /// notifications, those on paired Macs by asking those Macs to report.
    /// Folders that cannot be watched, such as unmounted disks or paired
    /// Macs while Mac-to-Mac is off, are returned with their pair. SFTP
    /// folders send no notifications; their changes are found by the
    /// periodic verification run.
    pub fn watch(&mut self, pairs: &[PairConfig], reach: &Reach) -> Vec<(PairId, io::Error)> {
        let sides = pairs.iter().flat_map(|pair| {
            let draft = pair.draft();
            [(draft.left(), pair.id()), (draft.right(), pair.id())]
        });
        let mut local = Vec::new();
        let mut remote: HashMap<PeerRoot, Vec<PairId>> = HashMap::new();
        for (root, id) in sides {
            match root {
                PairRoot::Local(root) => local.push((root.clone(), id)),
                PairRoot::Peer(root) => remote.entry(root.clone()).or_default().push(id),
                PairRoot::Sftp(_) => {}
            }
        }
        let mut unwatched: Vec<(PairId, io::Error)> = self
            .roots
            .set_roots(&local, reach.state())
            .into_iter()
            .filter_map(|(root, error)| {
                local
                    .iter()
                    .find(|(candidate, _)| *candidate == root)
                    .map(|(_, id)| (*id, error))
            })
            .collect();

        *self
            .peer_pairs
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = remote.clone();
        self.peers.retain(|root, _| remote.contains_key(root));
        let link = reach.link();
        for (root, ids) in remote {
            if self.peers.contains_key(&root) {
                continue;
            }
            let Some(link) = &link else {
                unwatched.extend(ids.into_iter().map(|id| (id, mac_to_mac_off())));
                continue;
            };
            let (pairs, changed, key) = (
                Arc::clone(&self.peer_pairs),
                Arc::clone(&self.changed),
                root.clone(),
            );
            let handle = watch_folder(link, &root, move || {
                // Copied out, so the scheduler is never called under this lock.
                let ids = pairs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                for id in ids {
                    changed(id);
                }
            });
            self.peers.insert(root, handle);
        }
        unwatched
    }
}

fn mac_to_mac_off() -> io::Error {
    io::Error::other("Mac-to-Mac is off, so the other Mac cannot report changes")
}
