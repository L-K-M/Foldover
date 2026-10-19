//! Folders this agent shares, each with one paired Mac (ADR 0031).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_db::ShareRow;
use foldover_types::{DeviceId, LocalRoot, ShareId};

use super::{Error, PairStore};

/// A folder one paired Mac may sync with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Share {
    pub id: ShareId,
    pub device: DeviceId,
    pub root: LocalRoot,
    /// What the other Mac shows, e.g. "Photos".
    pub name: String,
    pub created: SystemTime,
}

impl PairStore {
    /// Share `root` with a paired device.
    pub fn add_share(
        &mut self,
        device: DeviceId,
        root: &LocalRoot,
        name: &str,
        created: SystemTime,
    ) -> Result<Share, Error> {
        let created_ms = created.duration_since(UNIX_EPOCH).map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
        share(
            self.database
                .create_share(device.as_bytes(), root.as_bytes(), name, created_ms)?,
        )
    }

    /// Stop sharing; whether the share existed.
    pub fn remove_share(&mut self, id: ShareId) -> Result<bool, Error> {
        Ok(self.database.remove_share(id.as_bytes())?)
    }

    pub fn shares(&self) -> Result<Vec<Share>, Error> {
        self.database.shares()?.into_iter().map(share).collect()
    }

    /// What `device` may reach.
    pub fn shares_for(&self, device: DeviceId) -> Result<Vec<Share>, Error> {
        self.database
            .shares_for(device.as_bytes())?
            .into_iter()
            .map(share)
            .collect()
    }
}

fn share(row: ShareRow) -> Result<Share, Error> {
    let id = <[u8; ShareId::BYTE_LEN]>::try_from(row.id).map_err(invalid("share id"))?;
    let device =
        <[u8; DeviceId::BYTE_LEN]>::try_from(row.device).map_err(invalid("share device"))?;
    let created_ms = u64::try_from(row.created_ms).map_err(invalid("share created time"))?;
    Ok(Share {
        id: ShareId::from_bytes(id),
        device: DeviceId::from_bytes(device),
        root: LocalRoot::from_bytes(row.root).map_err(invalid("share root"))?,
        name: row.name,
        created: UNIX_EPOCH + Duration::from_millis(created_ms),
    })
}

fn invalid<E>(what: &'static str) -> impl Fn(E) -> Error {
    move |_| Error::InvalidStored(what)
}
