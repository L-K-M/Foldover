//! Folders this agent shares, each with one paired device (ADR 0031).

use rusqlite::params;

use super::{Database, Error};

const SHARE_COLUMNS: &str = "id,device,root,name,created_ms";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareRow {
    pub id: Vec<u8>,
    pub device: Vec<u8>,
    pub root: Vec<u8>,
    pub name: String,
    pub created_ms: i64,
}

impl Database {
    /// Share `root` with a paired device under a new ID.
    pub fn create_share(
        &mut self,
        device: &[u8],
        root: &[u8],
        name: &str,
        created_ms: i64,
    ) -> Result<ShareRow, Error> {
        let row = ShareRow {
            id: uuid::Uuid::new_v4().into_bytes().to_vec(),
            device: device.to_vec(),
            root: root.to_vec(),
            name: name.to_owned(),
            created_ms,
        };
        self.connection.execute(
            &format!("INSERT INTO share ({SHARE_COLUMNS}) VALUES (?1,?2,?3,?4,?5)"),
            params![row.id, row.device, row.root, row.name, row.created_ms],
        )?;
        Ok(row)
    }

    /// Stop sharing; whether the share existed.
    pub fn remove_share(&mut self, id: &[u8]) -> Result<bool, Error> {
        Ok(self
            .connection
            .execute("DELETE FROM share WHERE id=?1", params![id])?
            > 0)
    }

    /// Every share, by name.
    pub fn shares(&self) -> Result<Vec<ShareRow>, Error> {
        self.query_shares("", params![])
    }

    /// The shares one device may reach, by name.
    pub fn shares_for(&self, device: &[u8]) -> Result<Vec<ShareRow>, Error> {
        self.query_shares("WHERE device=?1", params![device])
    }

    fn query_shares(
        &self,
        filter: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<ShareRow>, Error> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {SHARE_COLUMNS} FROM share {filter} ORDER BY name COLLATE NOCASE,id"
        ))?;
        let rows = statement.query_map(values, |row| {
            Ok(ShareRow {
                id: row.get(0)?,
                device: row.get(1)?,
                root: row.get(2)?,
                name: row.get(3)?,
                created_ms: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::DeviceRow;
    use super::super::tests::location;
    use super::*;

    #[test]
    fn shares_belong_to_one_paired_device_and_end_with_it() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let device = |id: u8| DeviceRow {
            id: vec![id; 32],
            name: "Mac".into(),
            paired_ms: 1,
        };
        database.put_device(&device(1)).unwrap();
        database.put_device(&device(2)).unwrap();

        let photos = database
            .create_share(&[1; 32], b"/Users/a/Photos", "Photos", 7)
            .unwrap();
        let music = database
            .create_share(&[2; 32], b"/Users/a/Music", "music", 8)
            .unwrap();
        assert_eq!(database.shares().unwrap(), [music.clone(), photos.clone()]);
        assert_eq!(
            database.shares_for(&[1; 32]).unwrap(),
            std::slice::from_ref(&photos)
        );

        // Only paired devices, absolute roots and real names.
        for (device, root, name) in [
            (&[3; 32][..], &b"/x"[..], "X"),
            (&[1; 32], b"relative", "X"),
            (&[1; 32], b"/nul\0", "X"),
            (&[1; 32], b"/x", " "),
        ] {
            assert!(database.create_share(device, root, name, 0).is_err());
        }

        assert!(database.remove_share(&photos.id).unwrap());
        assert!(!database.remove_share(&photos.id).unwrap());
        assert!(database.remove_device(&[2; 32]).unwrap());
        assert_eq!(database.shares().unwrap(), []);
    }
}
