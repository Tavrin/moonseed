//! Pure Gregorian conversion and PUC 5.4.9's C99/C-locale date formatting.
//!
//! All authority (local offset and inverse selection, including DST folds/gaps)
//! is supplied by the caller. No clock, locale, timezone database or Lua state.
//! The accepted range follows Linux's signed 32-bit `tm_year`, not 32-bit time_t.

const DAY: i64 = 86_400;
const MIN_YEAR: i64 = i32::MIN as i64 + 1900;
const MAX_YEAR: i64 = i32::MAX as i64 + 1900;

/// The nine fields written by both os.date("*t") and os.time(table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
    /// Sunday = 1, Saturday = 7.
    pub wday: u8,
    /// January 1 = 1.
    pub yday: u16,
    pub isdst: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CivilError {
    Missing(&'static str),
    NonInteger(&'static str),
    OutOfBound(&'static str),
    DateRange,
    /// A successful conversion to -1 is still rejected by PUC. The caller must
    /// write these fields back before raising the error, just as PUC does.
    TimeRange {
        normalized: Option<Civil>,
    },
    /// Entire remaining C string after '%', not just the offending character.
    InvalidConversion(Vec<u8>),
}

impl CivilError {
    /// Library wording only; the Lua diagnostic authority adds location/args.
    pub(crate) fn message(&self) -> Vec<u8> {
        match self {
            Self::Missing(f) => format!("field '{f}' missing in date table").into_bytes(),
            Self::NonInteger(f) => format!("field '{f}' is not an integer").into_bytes(),
            Self::OutOfBound(f) => format!("field '{f}' is out-of-bound").into_bytes(),
            Self::DateRange => b"date result cannot be represented in this installation".to_vec(),
            Self::TimeRange { .. } => {
                b"time result cannot be represented in this installation".to_vec()
            }
            Self::InvalidConversion(s) => {
                let mut out = b"invalid conversion specifier '%".to_vec();
                out.extend_from_slice(s);
                out.push(b'\'');
                out
            }
        }
    }
}

/// The embedding lane performs Lua's lua_tointegerx coercion first (including
/// numeric strings and integral floats), then passes this lossless classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Field {
    #[default]
    Missing,
    Integer(i64),
    NonInteger,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TimeFields {
    pub year: Field,
    pub month: Field,
    pub day: Field,
    pub hour: Field,
    pub min: Field,
    pub sec: Field,
    /// None is Lua nil (tm_isdst = -1); non-nil Lua truth is supplied by caller.
    pub isdst: Option<bool>,
}

fn field(
    value: Field,
    name: &'static str,
    default: Option<i64>,
    delta: i64,
) -> Result<i64, CivilError> {
    let n = match value {
        Field::Missing => return default.ok_or(CivilError::Missing(name)),
        Field::NonInteger => return Err(CivilError::NonInteger(name)),
        Field::Integer(n) => n,
    };
    if n < i32::MIN as i64 + delta || n > i32::MAX as i64 + delta {
        return Err(CivilError::OutOfBound(name));
    }
    Ok(n - delta)
}

fn leap(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// Days since 1970-01-01. March-based Gregorian eras avoid negative-year special
/// cases; i128 keeps every intermediate safe even for an arbitrary i64 year.
fn days(year: i64, month: i64, day: i64) -> i128 {
    let y = year as i128 - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yo = y - era * 400;
    let m = month as i128 + if month > 2 { -3 } else { 9 };
    era * 146_097 + yo * 365 + yo / 4 - yo / 100 + (153 * m + 2) / 5 + day as i128 - 1 - 719_468
}

/// UTC instant to normalized civil fields; rejects exactly when glibc gmtime's
/// tm_year cannot represent the result, including both ends of i64 seconds.
pub(crate) fn from_utc(seconds: i64) -> Result<Civil, CivilError> {
    let d = seconds.div_euclid(DAY);
    let z = d + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yo = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yo + era * 400;
    let doy = doe - (365 * yo + yo / 4 - yo / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(CivilError::DateRange);
    }
    let time = seconds.rem_euclid(DAY);
    Ok(Civil {
        year,
        month: month as u8,
        day: day as u8,
        hour: (time / 3600) as u8,
        min: (time / 60 % 60) as u8,
        sec: (time % 60) as u8,
        wday: ((d + 4).rem_euclid(7) + 1) as u8,
        yday: (d as i128 - days(year, 1, 1) + 1) as u16,
        isdst: false,
    })
}

/// Inverse UTC conversion. Redundant weekday/year-day/DST fields are ignored;
/// date and time must already be normalized. Unlike os.time, -1 is valid here.
pub(crate) fn to_utc(c: &Civil) -> Result<i64, CivilError> {
    let month_days = match c.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap(c.year) {
                29
            } else {
                28
            }
        }
        _ => return Err(CivilError::DateRange),
    };
    if !(MIN_YEAR..=MAX_YEAR).contains(&c.year)
        || c.day == 0
        || c.day > month_days
        || c.hour > 23
        || c.min > 59
        || c.sec > 59
    {
        return Err(CivilError::DateRange);
    }
    i64::try_from(
        days(c.year, c.month as i64, c.day as i64) * DAY as i128
            + c.hour as i128 * 3600
            + c.min as i128 * 60
            + c.sec as i128,
    )
    .map_err(|_| CivilError::DateRange)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalOffset<'a> {
    /// Seconds east of UTC.
    pub seconds: i32,
    pub isdst: bool,
    /// C-locale %Z spelling supplied by the local-time oracle.
    pub name: &'a [u8],
}

pub(crate) const UTC: LocalOffset<'static> = LocalOffset {
    seconds: 0,
    isdst: false,
    name: b"UTC",
};

/// Pure difftime without overflow or premature rounding of either instant.
pub(crate) fn difftime(a: i64, b: i64) -> f64 {
    (a as i128 - b as i128) as f64
}

pub(crate) fn local_civil(seconds: i64, offset: LocalOffset<'_>) -> Result<Civil, CivilError> {
    let mut c = from_utc(
        seconds
            .checked_add(offset.seconds as i64)
            .ok_or(CivilError::DateRange)?,
    )?;
    c.isdst = offset.isdst;
    Ok(c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedTime {
    pub seconds: i64,
    pub civil: Civil,
}

/// PUC's field order/defaults/bounds followed by Gregorian normalization.
/// `inverse` receives the normalized wall time expressed as UTC seconds and
/// the DST hint, and chooses a UTC instant. `offset` then supplies the actual
/// offset at that instant. This permits host-specific fold/gap search without
/// baking a timezone policy into the core. Both closures may report errors.
pub(crate) fn normalize_time<'a>(
    fields: TimeFields,
    mut inverse: impl FnMut(i64, Option<bool>) -> Result<i64, CivilError>,
    mut offset: impl FnMut(i64) -> Result<LocalOffset<'a>, CivilError>,
) -> Result<NormalizedTime, CivilError> {
    let year = field(fields.year, "year", None, 1900)? + 1900;
    let month = field(fields.month, "month", None, 1)?;
    let day = field(fields.day, "day", None, 0)?;
    let hour = field(fields.hour, "hour", Some(12), 0)?;
    let min = field(fields.min, "min", Some(0), 0)?;
    let sec = field(fields.sec, "sec", Some(0), 0)?;
    let year = year + month.div_euclid(12);
    let month = month.rem_euclid(12) + 1;
    let wall =
        days(year, month, day) * DAY as i128 + hour as i128 * 3600 + min as i128 * 60 + sec as i128;
    let failed = || CivilError::TimeRange { normalized: None };
    let wall = i64::try_from(wall).map_err(|_| failed())?;
    let seconds = inverse(wall, fields.isdst)?;
    let civil = local_civil(seconds, offset(seconds)?).map_err(|_| failed())?;
    if seconds == -1 {
        return Err(CivilError::TimeRange {
            normalized: Some(civil),
        });
    }
    Ok(NormalizedTime { seconds, civil })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DateOutput {
    Table(Civil),
    #[cfg_attr(not(test), allow(dead_code))]
    // Runtime streams strftime items; pure oracle uses the whole-buffer seam.
    Bytes(Vec<u8>),
}

/// Complete pure os.date seam. A leading ! selects UTC and %Z = GMT. PUC's
/// strcmp("*t") treats *t followed by NUL as a table request; ordinary literal
/// NULs in a format are preserved because the formatter uses the Lua length.
pub(crate) fn date<'a>(
    format: &[u8],
    seconds: i64,
    mut offset: impl FnMut(i64) -> Result<LocalOffset<'a>, CivilError>,
) -> Result<DateOutput, CivilError> {
    let (format, zone) = if let Some(f) = format.strip_prefix(b"!") {
        (
            f,
            LocalOffset {
                name: b"GMT",
                ..UTC
            },
        )
    } else {
        (format, offset(seconds)?)
    };
    let c = local_civil(seconds, zone)?;
    if format.split(|&b| b == 0).next() == Some(b"*t") {
        Ok(DateOutput::Table(c))
    } else {
        Ok(DateOutput::Bytes(strftime(format, &c, zone)?))
    }
}

const SHORT_DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const LONG_DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const LONG_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const SINGLE: &[u8] = b"aAbBcCdDeFgGhHIjmMnprRStTuUVwWxXyYzZ%";
const E_OPTIONS: &[u8] = b"cCxXyY";
const O_OPTIONS: &[u8] = b"deHImMSuUVwWy";

fn iso(c: &Civil) -> (i64, i64) {
    let dow = (c.wday as i64 + 5) % 7 + 1;
    let thursday = c.yday as i64 + 4 - dow;
    if thursday < 1 {
        let year = c.year - 1;
        (
            year,
            (thursday + if leap(year) { 366 } else { 365 } - 1) / 7 + 1,
        )
    } else if thursday > if leap(c.year) { 366 } else { 365 } {
        (c.year + 1, 1)
    } else {
        (c.year, (thursday - 1) / 7 + 1)
    }
}

/// Exact PUC C99 whitelist, with glibc C-locale expansions. Input is normalized
/// Civil (validated here); output is bytes, preserving literal non-UTF-8/NUL.
pub(crate) fn strftime(
    format: &[u8],
    c: &Civil,
    zone: LocalOffset<'_>,
) -> Result<Vec<u8>, CivilError> {
    to_utc(c)?;
    if !(1..=7).contains(&c.wday) || !(1..=366).contains(&c.yday) {
        return Err(CivilError::DateRange);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < format.len() {
        if format[i] != b'%' {
            out.push(format[i]);
            i += 1;
            continue;
        }
        i += 1;
        let start = i;
        let mut conv = format.get(i).copied().unwrap_or(0);
        if conv == b'E' || conv == b'O' {
            let options = if conv == b'E' { E_OPTIONS } else { O_OPTIONS };
            conv = format.get(i + 1).copied().unwrap_or(0);
            if options.contains(&conv) {
                i += 2;
            } else {
                conv = 0;
            }
        } else if SINGLE.contains(&conv) {
            i += 1;
        } else {
            conv = 0;
        }
        if conv == 0 {
            return Err(CivilError::InvalidConversion(
                format[start..]
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or_default()
                    .to_vec(),
            ));
        }
        // glibc formats tm_year+1900 with signed-int wrap for %Y/%C, but
        // calculates %y from tm_year modulo 100. Preserve those edge bytes.
        let year = (c.year as i32) as i64;
        let (iy, iw) = iso(c);
        let iy = (iy as i32) as i64;
        let hour12 = if c.hour.is_multiple_of(12) {
            12
        } else {
            c.hour % 12
        };
        let text = match conv {
            b'a' => SHORT_DAYS[c.wday as usize - 1].to_owned(),
            b'A' => LONG_DAYS[c.wday as usize - 1].to_owned(),
            b'b' | b'h' => SHORT_MONTHS[c.month as usize - 1].to_owned(),
            b'B' => LONG_MONTHS[c.month as usize - 1].to_owned(),
            b'C' => year.div_euclid(100).to_string(),
            b'd' => format!("{:02}", c.day),
            b'e' => format!("{:2}", c.day),
            b'F' => format!("{year}-{:02}-{:02}", c.month, c.day),
            b'g' => format!("{:02}", iy.rem_euclid(100)),
            b'G' => iy.to_string(),
            b'H' => format!("{:02}", c.hour),
            b'I' => format!("{hour12:02}"),
            b'j' => format!("{:03}", c.yday),
            b'm' => format!("{:02}", c.month),
            b'M' => format!("{:02}", c.min),
            b'n' => "\n".to_owned(),
            b'p' => if c.hour < 12 { "AM" } else { "PM" }.to_owned(),
            b'S' => format!("{:02}", c.sec),
            b't' => "\t".to_owned(),
            b'u' => ((c.wday as i64 + 5) % 7 + 1).to_string(),
            b'U' => format!("{:02}", (c.yday as i64 - 1 + 7 - (c.wday as i64 - 1)) / 7),
            b'V' => format!("{iw:02}"),
            b'w' => (c.wday - 1).to_string(),
            b'W' => format!(
                "{:02}",
                (c.yday as i64 - 1 + 7 - (c.wday as i64 + 5) % 7) / 7
            ),
            b'y' => format!("{:02}", c.year.rem_euclid(100)),
            b'Y' => year.to_string(),
            b'z' => {
                let minutes = (zone.seconds as i64).abs() / 60;
                format!(
                    "{}{:02}{:02}",
                    if zone.seconds < 0 { '-' } else { '+' },
                    minutes / 60,
                    minutes % 60
                )
            }
            b'Z' => {
                let name = zone.name.split(|&b| b == 0).next().unwrap_or_default();
                // PUC gives each strftime item a 250-byte buffer including NUL;
                // a name that does not fit produces zero output for this item.
                if name.len() < 250 {
                    out.extend_from_slice(name);
                }
                continue;
            }
            b'%' => "%".to_owned(),
            b'c' | b'D' | b'r' | b'R' | b'T' | b'x' | b'X' => {
                let expansion: &[u8] = match conv {
                    b'c' => b"%a %b %e %H:%M:%S %Y",
                    b'D' | b'x' => b"%m/%d/%y",
                    b'r' => b"%I:%M:%S %p",
                    b'R' => b"%H:%M",
                    _ => b"%H:%M:%S",
                };
                out.extend_from_slice(&strftime(expansion, c, zone)?);
                continue;
            }
            _ => unreachable!("whitelist and expansions agree"),
        };
        out.extend_from_slice(text.as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
#[path = "civil_tests.rs"]
mod tests;
