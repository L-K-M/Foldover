//! Macs this agent paired with (ADR 0031).

use rusqlite::params;

use super::{Database, Error};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRow {
    pub id: Vec<u8>,
    pub name: String,
    pub paired_ms: i64,
}

impl Database {
    /// Record a paired device, replacing an earlier pairing with it.
    pub fn put_device(&mut self, row: &DeviceRow) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO device (id,name,paired_ms) VALUES (?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,paired_ms=excluded.paired_ms",
            params![row.id, row.name, row.paired_ms],
        )?;
        Ok(())
    }

    /// Forget a device; whether it was known.
    pub fn remove_device(&mut self, id: &[u8]) -> Result<bool, Error> {
        Ok(self
            .connection
            .execute("DELETE FROM device WHERE id=?1", params![id])?
            > 0)
    }

    /// Paired devices, by name.
    pub fn devices(&self) -> Result<Vec<DeviceRow>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT id,name,paired_ms FROM device ORDER BY name COLLATE NOCASE,id")?;
        let rows = statement.query_map([], |row| {
            Ok(DeviceRow {
                id: row.get(0)?,
                name: row.get(1)?,
                paired_ms: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn devices_are_recorded_renamed_and_forgotten() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        let row = |id: u8, name: &str| DeviceRow {
            id: vec![id; 32],
            name: name.into(),
            paired_ms: 5,
        };
        database.put_device(&row(2, "Studio")).unwrap();
        database.put_device(&row(1, "Laptop")).unwrap();
        database.put_device(&row(2, "iMac")).unwrap();
        assert_eq!(
            database.devices().unwrap(),
            [row(2, "iMac"), row(1, "Laptop")]
        );

        assert!(database.remove_device(&[1; 32]).unwrap());
        assert!(!database.remove_device(&[1; 32]).unwrap());
        assert!(database.put_device(&row(3, " ")).is_err());
        assert!(
            database
                .put_device(&DeviceRow {
                    id: vec![4; 31],
                    ..row(4, "Short")
                })
                .is_err()
        );
    }
}
