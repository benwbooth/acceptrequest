//! Windows logon hours: a 168-bit weekly map, one bit per hour starting Sunday 00:00 UTC.
//! The service mirrors the schedule into it so Windows itself refuses sign-in during downtime
//! or after the day's time is used up, even if the service isn't running.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Timelike, Utc};

pub const BYTES: usize = 21;

/// Index of the hour that starts at `t` in Windows' weekly map.
fn hour_of_week(t: DateTime<Utc>) -> usize {
    t.weekday().num_days_from_sunday() as usize * 24 + t.hour() as usize
}

/// The weekly map for the next 168 hours from `now`. An hour is allowed if the user may be
/// signed in at any quarter of it (Windows can't express finer limits; the service enforces
/// the exact minute). `to_local` converts to the local wall clock the schedule is written in.
pub fn weekly_map(
    now: DateTime<Utc>,
    to_local: impl Fn(DateTime<Utc>) -> NaiveDateTime,
    allowed_at: impl Fn(NaiveDateTime) -> bool,
) -> [u8; BYTES] {
    let start = now.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0)).unwrap_or(now);
    let mut map = [0u8; BYTES];
    for k in 0..168 {
        let hour = start + Duration::hours(k);
        let allowed = (0..4).any(|q| {
            let t = hour + Duration::minutes(q * 15);
            // The part of the current hour that already passed doesn't matter.
            t + Duration::minutes(15) > now && allowed_at(to_local(t))
        });
        if allowed {
            let i = hour_of_week(hour);
            map[i / 8] |= 1 << (i % 8);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveTime, TimeZone};

    fn bit(map: &[u8; BYTES], i: usize) -> bool {
        map[i / 8] & (1 << (i % 8)) != 0
    }

    #[test]
    fn downtime_maps_to_utc_hours() {
        // Pacific time (UTC-8). Monday 2026-11-02 12:00 local = 20:00 UTC.
        let pst = FixedOffset::west_opt(8 * 3600).unwrap();
        let now = pst.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap().with_timezone(&Utc);
        // Downtime every night 21:00-07:00 local.
        let night = |t: NaiveDateTime| {
            let h = t.time();
            !(h >= NaiveTime::from_hms_opt(21, 0, 0).unwrap() || h < NaiveTime::from_hms_opt(7, 0, 0).unwrap())
        };
        let map = weekly_map(now, |t| t.with_timezone(&pst).naive_local(), night);
        // Monday 20:00 local = Tuesday 04:00 UTC -> allowed; Monday 22:00 local = Tue 06:00 UTC -> blocked.
        assert!(bit(&map, 2 * 24 + 4));
        assert!(!bit(&map, 2 * 24 + 6));
        // Tuesday 07:00 local = Tuesday 15:00 UTC -> allowed again.
        assert!(bit(&map, 2 * 24 + 15));
        // 14 of every 24 hours are allowed.
        assert_eq!(map.iter().map(|b| b.count_ones()).sum::<u32>(), 7 * 14);
    }

    #[test]
    fn partial_hours_and_the_current_hour() {
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 10, 50, 0).unwrap(); // Monday
        // Only allowed until 10:30 today, then blocked forever.
        let cutoff = now.naive_utc().date().and_hms_opt(10, 30, 0).unwrap();
        let map = weekly_map(now, |t| t.naive_utc(), |t| t < cutoff);
        // 10:00-10:30 already passed, so the current hour is blocked too.
        assert_eq!(map, [0u8; BYTES]);

        let now = Utc.with_ymd_and_hms(2026, 11, 2, 10, 10, 0).unwrap();
        let map = weekly_map(now, |t| t.naive_utc(), |t| t < cutoff);
        assert!(bit(&map, 24 + 10));
        assert_eq!(map.iter().map(|b| b.count_ones()).sum::<u32>(), 1);
    }
}
