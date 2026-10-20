//! Local wall-clock time from the C library, for calendar schedules
//! (PLAN.md §3.5). `localtime_r` is thread-safe on Linux and macOS and
//! follows the system time zone, unlike the shared-state `localtime`.

use std::time::{SystemTime, UNIX_EPOCH};

use foldover_types::{LocalDateTime, Weekday};

// The libc crate declares tzset for Windows only; POSIX has it everywhere.
unsafe extern "C" {
    fn tzset();
}

/// `at` broken down in the system time zone; `None` when the C library
/// cannot answer. Pre-epoch times report as the epoch itself.
pub(crate) fn local_time(at: SystemTime) -> Option<LocalDateTime> {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let time = libc::time_t::try_from(seconds).ok()?;
    // POSIX lets localtime_r keep the zone the C library read last (glibc
    // does), so a Mac that moved to another time zone could keep the old
    // one until the agent restarts; tzset reads the current one first.
    // SAFETY: tzset takes no arguments and refreshes the C library's own
    // time zone state under its lock. It reads TZ, which this process
    // never changes, so no environment write can race it.
    unsafe { tzset() };
    let mut broken = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r fills `*result` and returns it on success; the
    // returned pointer is checked and the struct is read only then. All
    // tm fields are plain integers, so no aliasing or lifetime concerns.
    let written = unsafe { libc::localtime_r(&raw const time, broken.as_mut_ptr()) };
    if written.is_null() {
        return None;
    }
    // SAFETY: `written` is non-null, so localtime_r initialized `broken`.
    let broken = unsafe { broken.assume_init() };
    let weekday = match broken.tm_wday {
        0 => Weekday::Sunday,
        1 => Weekday::Monday,
        2 => Weekday::Tuesday,
        3 => Weekday::Wednesday,
        4 => Weekday::Thursday,
        5 => Weekday::Friday,
        _ => Weekday::Saturday,
    };
    Some(LocalDateTime::new(
        broken.tm_year + 1900,
        (broken.tm_mon + 1) as u8,
        broken.tm_mday as u8,
        weekday,
        (broken.tm_hour * 60 + broken.tm_min) as u16,
    ))
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use super::*;

    /// Set in the process the time zone test starts, where that test runs
    /// alone on one thread and may change TZ.
    const ALONE: &str = "FOLDOVER_CLOCK_TEST_ALONE";
    const TIME_ZONE_TEST: &str = "clock::tests::a_time_zone_change_is_followed";

    #[test]
    fn the_epoch_reports_a_thursday() {
        let local = local_time(UNIX_EPOCH).unwrap();
        // UTC and zones east of it see Thursday 1970-01-01; zones far
        // west still see Wednesday evening. Both prove real conversion.
        assert!(matches!(
            local.weekday(),
            Weekday::Wednesday | Weekday::Thursday
        ));
        assert_eq!(local.minute() % 60, 0);
        let now = local_time(SystemTime::now()).unwrap();
        assert!(now > local);
    }

    /// Review: a Mac that changed time zone kept firing daily syncs on the
    /// old zone's clock until the agent restarted.
    #[test]
    fn a_time_zone_change_is_followed() {
        if std::env::var_os(ALONE).is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([TIME_ZONE_TEST, "--exact", "--test-threads=1"])
                .env(ALONE, "1")
                .env("TZ", "UTC0")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let noon = UNIX_EPOCH + Duration::from_secs(12 * 60 * 60);
        assert_eq!(local_time(noon).unwrap().minute(), 12 * 60);
        // SAFETY: this process runs this one test on one thread, so
        // nothing reads the environment while it changes.
        unsafe { std::env::set_var("TZ", "JST-9") };
        assert_eq!(local_time(noon).unwrap().minute(), 21 * 60);
    }
}
