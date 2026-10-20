//! The person's automation choices, for the whole agent. No row means
//! every setting's default.

use rusqlite::{OptionalExtension, params};

use super::{Database, Error};

/// One automation_setting row; `low_power` is `pause` or `run`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationSettingRow {
    pub low_power: String,
}

impl Database {
    pub fn automation_setting(&self) -> Result<Option<AutomationSettingRow>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT low_power FROM automation_setting WHERE singleton=1",
                [],
                |row| {
                    Ok(AutomationSettingRow {
                        low_power: row.get(0)?,
                    })
                },
            )
            .optional()?)
    }

    /// Set `low_power` (`pause` or `run`); the choice survives restarts.
    pub fn set_low_power(&mut self, low_power: &str) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO automation_setting (singleton,low_power) VALUES (1,?1)
             ON CONFLICT(singleton) DO UPDATE SET low_power=excluded.low_power",
            params![low_power],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::location;
    use super::*;

    #[test]
    fn the_low_power_choice_defaults_to_none_and_survives_reopening() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        assert_eq!(database.automation_setting().unwrap(), None);
        database.set_low_power("run").unwrap();
        database.set_low_power("pause").unwrap();
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "pause".into()
            })
        );
        drop(database);

        let mut database = Database::open(&location).unwrap();
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "pause".into()
            })
        );
        // The CHECK constraint rejects a value state cannot decode.
        assert!(database.set_low_power("sometimes").is_err());
    }
}
