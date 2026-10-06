//! Fixed US DST rules for the frozen fixture corpus, not a timezone database.
//! Selected explicitly by --host-civil=fixture-tz and TZ=America/New_York.
use moonseed::{
    CapabilityCompletion as Completion, CivilOffset, CivilTime, HostIoError, HostIoErrorKind,
};
pub struct FixtureNewYork;
const DAY: i64 = 86400;
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yo = year - era * 400;
    let month = month + if month > 2 { -3 } else { 9 };
    era * 146097 + yo * 365 + yo / 4 - yo / 100 + (153 * month + 2) / 5 + day - 1 - 719468
}
fn offset(seconds: i64) -> CivilOffset {
    let z = seconds.div_euclid(DAY) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yo = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yo + era * 400;
    let doy = doe - (365 * yo + yo / 4 - yo / 100);
    let mp = (5 * doy + 2) / 153;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let transition = |month: i64, nth: i64, hour: i64| {
        let first = days(year, month, 1);
        let sunday = 1 + (7 - (first + 4).rem_euclid(7)) % 7;
        let day = if nth > 0 {
            sunday + (nth - 1) * 7
        } else {
            let len = if month == 4 { 30 } else { 31 };
            sunday + (len - sunday) / 7 * 7
        };
        (first + day - 1) * DAY + hour * 3600
    };
    let (start, end) = if year >= 2007 {
        (transition(3, 2, 7), transition(11, 1, 6))
    } else if year >= 1967 {
        let start = match year {
            1974 => days(year, 1, 6) * DAY + 7 * 3600,
            1975 => days(year, 2, 23) * DAY + 7 * 3600,
            _ => transition(4, if year >= 1987 { 1 } else { -1 }, 7),
        };
        (start, transition(10, -1, 6))
    } else {
        (0, 0)
    };
    let isdst = seconds >= start && seconds < end;
    CivilOffset {
        seconds: if isdst { -14400 } else { -18000 },
        isdst,
    }
}
impl CivilTime for FixtureNewYork {
    fn local_offset(&self, seconds: i64) -> Completion<CivilOffset> {
        Completion::Ready(Ok(offset(seconds)))
    }
    fn zone_name(&self, seconds: i64) -> Completion<Vec<u8>> {
        Completion::Ready(Ok(if offset(seconds).isdst {
            b"EDT".to_vec()
        } else {
            b"EST".to_vec()
        }))
    }
    fn utc_seconds(&self, wall: i64, hint: Option<bool>) -> Completion<i64> {
        let result = (|| {
            let err =
                || HostIoError::new(HostIoErrorKind::InvalidInput, b"time out of range".to_vec());
            let standard = wall.checked_add(18000).ok_or_else(err)?;
            let daylight = wall.checked_add(14400).ok_or_else(err)?;
            Ok(match hint {
                Some(true) => daylight,
                Some(false) => standard,
                None if !offset(standard).isdst => standard,
                None if offset(daylight).isdst => daylight,
                None => standard,
            })
        })();
        Completion::Ready(result)
    }
}
