//! Macs this agent paired with (ADR 0031).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_db::DeviceRow;
use foldover_types::DeviceId;

use super::{Error, PairStore};

/// A paired Mac.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    /// The name it gave when paired, e.g. "Studio iMac".
    pub name: String,
    pub paired: SystemTime,
}

impl PairStore {
    /// Record a device after both people confirmed its pairing code.
    pub fn put_device(&mut self, device: &Device) -> Result<(), Error> {
        let paired_ms = device.paired.duration_since(UNIX_EPOCH).map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
        Ok(self.database.put_device(&DeviceRow {
            id: device.id.as_bytes().to_vec(),
            name: device.name.clone(),
            paired_ms,
        })?)
    }

    /// Forget a device; whether it was paired.
    pub fn remove_device(&mut self, id: DeviceId) -> Result<bool, Error> {
        Ok(self.database.remove_device(id.as_bytes())?)
    }

    pub fn devices(&self) -> Result<Vec<Device>, Error> {
        self.database
            .devices()?
            .into_iter()
            .map(|row| {
                let id = <[u8; DeviceId::BYTE_LEN]>::try_from(row.id)
                    .map_err(|_| Error::InvalidStored("device id"))?;
                let paired = UNIX_EPOCH
                    + Duration::from_millis(
                        u64::try_from(row.paired_ms)
                            .map_err(|_| Error::InvalidStored("device paired time"))?,
                    );
                Ok(Device {
                    id: DeviceId::from_bytes(id),
                    name: row.name,
                    paired,
                })
            })
            .collect()
    }
}
