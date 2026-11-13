//! Showing saved times in local time without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970-01-01 UTC, now.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Difference between local time and UTC in seconds, as the system has it
/// now. Zero where it cannot be asked.
pub fn utc_offset_seconds() -> i64 {
    platform::utc_offset_seconds()
}

/// Calendar date and time of day for a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTime {
    /// The moment `unix` seconds after 1970-01-01 UTC, shifted by `offset`.
    pub fn from_unix(unix: u64, offset: i64) -> Self {
        let seconds = unix as i64 + offset;
        let days = seconds.div_euclid(86_400);
        let of_day = seconds.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: of_day / 3_600,
            minute: of_day % 3_600 / 60,
            second: of_day % 60,
        }
    }

    /// Seconds since 1970-01-01 for this date and time taken as UTC.
    pub fn to_unix(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + i64::from(self.hour) * 3_600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}

/// `13.11.2026 14:05`, the form used in the call list.
pub fn format_timestamp(unix: u64, offset: i64) -> String {
    let time = DateTime::from_unix(unix, offset);
    format!(
        "{:02}.{:02}.{} {:02}:{:02}",
        time.day, time.month, time.year, time.hour, time.minute
    )
}

/// `1:05` or `1:02:03`, the length of a call.
pub fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3_600, seconds % 3_600 / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

// Howard Hinnant's algorithms for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(windows)]
mod platform {
    use super::{DateTime, now_unix};

    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }

    pub fn utc_offset_seconds() -> i64 {
        let mut local = SystemTime::default();
        // SAFETY: the function fills the structure it is given.
        unsafe { GetLocalTime(&mut local) };
        let local = DateTime {
            year: i64::from(local.year),
            month: u32::from(local.month),
            day: u32::from(local.day),
            hour: u32::from(local.hour),
            minute: u32::from(local.minute),
            second: u32::from(local.second),
        };
        // Time zones are whole quarters of an hour; rounding removes the
        // moment that passed between the two clock readings.
        let difference = local.to_unix() - now_unix() as i64;
        (difference as f64 / 900.0).round() as i64 * 900
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn utc_offset_seconds() -> i64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_moments_convert_to_calendar_dates() {
        let at = |unix: u64, offset: i64| {
            let time = DateTime::from_unix(unix, offset);
            (
                time.year,
                time.month,
                time.day,
                time.hour,
                time.minute,
                time.second,
            )
        };
        assert_eq!(at(0, 0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(at(951_782_400, 0), (2000, 2, 29, 0, 0, 0));
        assert_eq!(at(1_794_441_600, 0), (2026, 11, 12, 0, 0, 0));
        assert_eq!(at(1_794_441_599, 0), (2026, 11, 11, 23, 59, 59));
        // Moscow is three hours ahead: the date changes earlier there.
        assert_eq!(
            at(1_794_441_599 - 3 * 3_600, 3 * 3_600),
            (2026, 11, 11, 23, 59, 59)
        );
        assert_eq!(at(1_794_436_200, 3 * 3_600), (2026, 11, 12, 1, 30, 0));
        // An offset behind UTC, across the start of the epoch.
        assert_eq!(at(0, -5 * 3_600), (1969, 12, 31, 19, 0, 0));
    }

    #[test]
    fn dates_convert_back_to_the_same_moment() {
        for unix in [0_u64, 86_399, 951_782_400, 1_794_441_600, 4_102_444_800] {
            assert_eq!(DateTime::from_unix(unix, 0).to_unix(), unix as i64);
        }
        // Every day of a leap year and the years around it.
        let start = days_from_civil(2023, 1, 1);
        let mut previous = (2022, 12, 31);
        for day in start..start + 3 * 366 {
            let date = civil_from_days(day);
            assert_ne!(date, previous);
            assert_eq!(days_from_civil(date.0, date.1, date.2), day);
            previous = date;
        }
    }

    #[test]
    fn timestamps_and_durations_read_naturally() {
        assert_eq!(
            format_timestamp(1_794_436_200, 3 * 3_600),
            "12.11.2026 01:30"
        );
        assert_eq!(format_timestamp(0, 0), "01.01.1970 00:00");
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(65), "1:05");
        assert_eq!(format_duration(3_723), "1:02:03");
    }

    #[test]
    fn system_offset_is_a_plausible_time_zone() {
        let offset = utc_offset_seconds();
        assert!((-12 * 3_600..=14 * 3_600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0);
        assert!(now_unix() > 1_700_000_000);
    }
}
