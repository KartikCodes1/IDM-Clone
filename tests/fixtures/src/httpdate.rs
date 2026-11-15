//! IMF-fixdate (RFC 9110 section 5.6.7) formatting without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const SECS_PER_DAY: u64 = 86_400;

/// Formats `unix_secs` as `Sun, 06 Nov 1994 08:49:37 GMT`.
#[must_use]
pub fn imf_fixdate(unix_secs: u64) -> String {
    let days = unix_secs / SECS_PER_DAY;
    let rem = unix_secs % SECS_PER_DAY;
    // 1970-01-01 was a Thursday (index 4 with Sunday = 0).
    let weekday = WEEKDAYS[usize::try_from((days + 4) % 7).unwrap_or(0)];
    let (year, month, day) = civil_from_days(days);
    format!(
        "{weekday}, {day:02} {} {year:04} {:02}:{:02}:{:02} GMT",
        MONTHS[usize::try_from(month - 1).unwrap_or(0)],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The current time as an IMF-fixdate.
#[must_use]
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    imf_fixdate(secs)
}

/// Days since 1970-01-01 to (year, month 1-12, day 1-31); H. Hinnant's civil algorithm.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::imf_fixdate;

    #[test]
    fn formats_known_instants() {
        assert_eq!(imf_fixdate(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(imf_fixdate(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(imf_fixdate(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
        assert_eq!(imf_fixdate(4_102_444_799), "Thu, 31 Dec 2099 23:59:59 GMT");
    }
}
