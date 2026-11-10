//! The person's automation choices, for the whole agent. No row means
//! every setting's default.

use rusqlite::{OptionalExtension, params};

use super::{Database, Error};

/// One automation_setting row; `low_power` is `pause` or `run`, and
/// `automatic_effort` is `gentle` or `full`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationSettingRow {
    pub low_power: String,
    pub automatic_effort: String,
}

impl Database {
    pub fn automation_setting(&self) -> Result<Option<AutomationSettingRow>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT low_power, automatic_effort FROM automation_setting WHERE singleton=1",
                [],
                |row| {
                    Ok(AutomationSettingRow {
                        low_power: row.get(0)?,
                        automatic_effort: row.get(1)?,
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

    /// Set `automatic_effort` (`gentle` or `full`); the choice survives
    /// restarts. A first row takes the other settings' defaults.
    pub fn set_automatic_effort(&mut self, effort: &str) -> Result<(), Error> {
        self.connection.execute(
            "INSERT INTO automation_setting (singleton,low_power,automatic_effort)
             VALUES (1,'pause',?1)
             ON CONFLICT(singleton) DO UPDATE SET automatic_effort=excluded.automatic_effort",
            params![effort],
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
                low_power: "pause".into(),
                automatic_effort: "gentle".into(),
            })
        );
        drop(database);

        let mut database = Database::open(&location).unwrap();
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "pause".into(),
                automatic_effort: "gentle".into(),
            })
        );
        // The CHECK constraint rejects a value state cannot decode.
        assert!(database.set_low_power("sometimes").is_err());
    }

    #[test]
    fn the_effort_choice_keeps_the_low_power_choice_and_survives_reopening() {
        let (_directory, location) = location();
        let mut database = Database::open(&location).unwrap();
        // A first row written by the effort takes the low power default.
        database.set_automatic_effort("full").unwrap();
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "pause".into(),
                automatic_effort: "full".into(),
            })
        );
        database.set_low_power("run").unwrap();
        // The low power choice keeps the effort choice.
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "run".into(),
                automatic_effort: "full".into(),
            })
        );
        database.set_automatic_effort("gentle").unwrap();
        drop(database);

        let database = Database::open(&location).unwrap();
        assert_eq!(
            database.automation_setting().unwrap(),
            Some(AutomationSettingRow {
                low_power: "run".into(),
                automatic_effort: "gentle".into(),
            })
        );
        let mut database = database;
        assert!(database.set_automatic_effort("fast").is_err());
    }
}
