//! Mapping of the frozen hostlib corpus: date IDs encode timestamp, format and
//! leading-! flag; time IDs index the generator's 14 field tables. See the T1
//! report for exclusions (Lua argument validation and os_misc authority).
use super::*;
use std::fmt::Write;

const TIMESTAMPS: [i64; 8] = [
    -2208988800,
    -1,
    0,
    951782400,
    1583650800,
    1604210400,
    1700000000,
    2147483647,
];
const FORMATS: [&[u8]; 17] = [
    b"%Y-%m-%d %H:%M:%S",
    b"%a %A %b %B",
    b"%c",
    b"%d %e %j %m %w %u",
    b"%H %I %M %S %p",
    b"%U %W %V %G %g",
    b"%x %X %y %Y",
    b"%z %Z",
    b"%% %n %t",
    b"%D %F %r %R %T",
    b"",
    b"*t",
    b"!*t",
    b"%Q",
    b"%",
    b"%E",
    b"%O",
];
const NY_FORMATS: [&[u8]; 5] = [FORMATS[0], FORMATS[1], b"%z %Z", b"*t", b"!*t"];

/// Test-only fixed New York rules, not a timezone database. The corpus covers
/// standard time in 1900 and US rules in 1970/2000/2020/2023/2038. For 1967..2006
/// use historical April/October rules (plus the 1974/75 emergency exceptions).
fn new_york(seconds: i64) -> Result<LocalOffset<'static>, CivilError> {
    let year = from_utc(seconds)?.year;
    let transition = |month: i64, nth: i64, hour: i64| {
        let first = days(year, month, 1) as i64;
        let sunday = 1 + (7 - (first + 4).rem_euclid(7)) % 7;
        let day = if nth > 0 {
            sunday + (nth - 1) * 7
        } else {
            let length = if month == 4 { 30 } else { 31 };
            sunday + (length - sunday) / 7 * 7
        };
        (first + day - 1) * DAY + hour * 3600
    };
    let (start, end) = if year >= 2007 {
        (transition(3, 2, 7), transition(11, 1, 6))
    } else if year >= 1967 {
        let start = match year {
            1974 => days(year, 1, 6) as i64 * DAY + 7 * 3600,
            1975 => days(year, 2, 23) as i64 * DAY + 7 * 3600,
            _ => transition(4, if year >= 1987 { 1 } else { -1 }, 7),
        };
        (start, transition(10, -1, 6))
    } else {
        (0, 0)
    };
    let dst = seconds >= start && seconds < end;
    Ok(LocalOffset {
        seconds: if dst { -14_400 } else { -18_000 },
        isdst: dst,
        name: if dst { b"EDT" } else { b"EST" },
    })
}

fn utc_inverse(wall: i64, hint: Option<bool>) -> Result<i64, CivilError> {
    wall.checked_sub(if hint == Some(true) { 3600 } else { 0 })
        .ok_or(CivilError::TimeRange { normalized: None })
}

fn ny_inverse(wall: i64, hint: Option<bool>) -> Result<i64, CivilError> {
    let daylight = wall
        .checked_add(14_400)
        .ok_or(CivilError::TimeRange { normalized: None })?;
    let standard = wall
        .checked_add(18_000)
        .ok_or(CivilError::TimeRange { normalized: None })?;
    Ok(match hint {
        Some(true) => daylight,
        Some(false) => standard,
        None if new_york(standard)?.seconds == -18_000 => standard,
        None if new_york(daylight)?.seconds == -14_400 => daylight,
        None => standard, // deterministic gap policy, supplied by the oracle
    })
}

fn table(index: usize) -> TimeFields {
    use Field::{Integer as I, Missing as M, NonInteger as N};
    let mut f = TimeFields {
        year: I(2020),
        month: I(1),
        day: I(1),
        isdst: Some(false),
        ..TimeFields::default()
    };
    match index {
        0 => {
            f.year = I(1970);
            f.hour = I(0);
        }
        1 => {
            f.year = I(2000);
            f.month = I(2);
            f.day = I(29);
            f.hour = I(0);
        }
        2 => {
            f.month = I(13);
            f.day = I(0);
            f.hour = I(25);
            f.min = I(-2);
            f.sec = I(70);
        }
        3 => {
            f.month = I(0);
            f.day = I(40);
            f.hour = I(-1);
        }
        4 => {
            f.month = I(3);
            f.day = I(8);
            f.hour = I(2);
            f.min = I(30);
        }
        5 | 6 => {
            f.month = I(11);
            f.hour = I(1);
            f.min = I(30);
            f.isdst = Some(index == 6);
        }
        7 | 10 => {} // index 10 is "2020", already lua_tointegerx-coerced
        8 => {
            f.year = M;
            f.month = M;
            f.day = M;
        }
        9 => f.day = M,
        11 => f.year = N,
        12 => f.year = I(i64::MAX),
        13 => f.month = N,
        _ => panic!("unknown frozen table"),
    }
    f
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}

fn civil_values(c: Civil) -> Vec<(&'static str, String)> {
    let mut out: Vec<_> = [
        c.year,
        c.month as i64,
        c.day as i64,
        c.hour as i64,
        c.min as i64,
        c.sec as i64,
        c.wday as i64,
        c.yday as i64,
    ]
    .into_iter()
    .map(|n| ("number.integer", n.to_string()))
    .collect();
    out.push(("boolean", c.isdst.to_string()));
    out
}

fn record(id: &str, result: Result<Vec<(&str, String)>, CivilError>, date_error: bool) -> String {
    match result {
        Ok(values) => {
            let mut line = format!("{id}\tok\t{}", values.len());
            for (ty, v) in values {
                write!(line, "\t{ty}\t{v}").unwrap();
            }
            line.push_str("\t-\t-");
            line
        }
        Err(e) => {
            let mut msg = b"hostlib-case.lua:1: ".to_vec();
            if date_error {
                msg.extend_from_slice(b"bad argument #1 to 'date' (");
            }
            msg.extend_from_slice(&e.message());
            if date_error {
                msg.push(b')');
            }
            format!("{id}\terror\t0\tstring\t{}", hex(&msg))
        }
    }
}

#[test]
fn frozen_corpus_records() {
    let mut checked = 0;
    let mut excluded = Vec::new();
    for (ny, corpus) in [
        (false, include_str!("../fixtures/civil/os_utc.txt")),
        (true, include_str!("../fixtures/civil/os_new_york.txt")),
    ] {
        for want in corpus.lines() {
            let id = want.split('\t').next().unwrap();
            let parts: Vec<_> = id.split('.').collect();
            let got = if parts[1] == "date" && parts[2] != "arg" {
                let ti: usize = parts[2].parse().unwrap();
                let fi: usize = parts[3].parse().unwrap();
                let mut fmt = if ny { NY_FORMATS[fi] } else { FORMATS[fi] }.to_vec();
                if parts[4] == "1" {
                    fmt.insert(0, b'!');
                }
                let result = date(
                    &fmt,
                    TIMESTAMPS[ti],
                    |n| if ny { new_york(n) } else { Ok(UTC) },
                );
                let invalid = matches!(result, Err(CivilError::InvalidConversion(_)));
                record(
                    id,
                    result.map(|x| match x {
                        DateOutput::Table(c) => civil_values(c),
                        DateOutput::Bytes(b) => vec![("string", hex(&b))],
                    }),
                    invalid,
                )
            } else if parts[1] == "time" {
                let f = table(parts[2].parse().unwrap());
                let result = if ny {
                    normalize_time(f, ny_inverse, new_york)
                } else {
                    normalize_time(f, utc_inverse, |_| Ok(UTC))
                };
                record(
                    id,
                    result.map(|n| {
                        let mut values = vec![("number.integer", n.seconds.to_string())];
                        values.extend(civil_values(n.civil));
                        values
                    }),
                    false,
                )
            } else if parts[1] == "date" && parts[3] == "3" {
                record(
                    id,
                    date(b"!%Y", i64::MAX, |_| {
                        panic!("UTC must not query local time")
                    })
                    .map(|_| Vec::new()),
                    false,
                )
            } else if parts[1] == "diff" && parts[2].parse::<usize>().unwrap() < 5 {
                // Compare the frozen Lua %a payload numerically; the existing
                // Lua number formatter, not this module, owns float hex output.
                let i = parts[2].parse::<usize>().unwrap();
                let (a, b) = [(0, 0), (1, 0), (-1, 1), (1700000000, 951782400), (3, 1)][i];
                let payload = want.split('\t').nth(4).unwrap();
                let (mantissa, exponent) = payload.split_once('p').unwrap();
                let negative = mantissa.starts_with('-');
                let mantissa = mantissa.trim_start_matches('-').trim_start_matches("0x");
                let value = mantissa.bytes().filter(|&b| b != b'.').fold(0.0, |n, b| {
                    n * 16.0 + (b as char).to_digit(16).unwrap() as f64
                });
                let fractional = mantissa.split_once('.').map_or(0, |(_, tail)| tail.len());
                let value =
                    value * 2.0f64.powi(exponent.parse::<i32>().unwrap() - fractional as i32 * 4);
                assert_eq!(
                    difftime(a, b),
                    if negative { -value } else { value },
                    "{id}"
                );
                checked += 1;
                continue;
            } else {
                excluded.push(id.to_owned());
                continue;
            };
            assert_eq!(got, want, "{id}");
            checked += 1;
        }
    }
    assert_eq!(checked, 338);
    assert_eq!(
        excluded,
        [
            "os_utc.diff.5",
            "os_utc.diff.6",
            "os_utc.date.arg.0",
            "os_utc.date.arg.1",
            "os_utc.date.arg.2"
        ]
    );
}

#[test]
fn oracle_all_conversions_and_extreme_years() {
    // Independently captured from the pinned PUC executable in TZ=UTC, LC_ALL=C.
    // Each record stores an explicit instant, byte format, and exact output.
    for line in include_str!("../fixtures/civil/civil-t1.txt").lines() {
        let mut parts = line.split('\t');
        let seconds: i64 = parts.next().unwrap().parse().unwrap();
        let format = unhex(parts.next().unwrap());
        let expected = unhex(parts.next().unwrap());
        assert_eq!(
            date(&format, seconds, |_| Ok(UTC)).unwrap(),
            DateOutput::Bytes(expected.clone()),
            "instant={seconds}; expected={}",
            String::from_utf8_lossy(&expected)
        );
    }
}

#[test]
fn range_roundtrip_and_table_bounds() {
    let min = -67_768_040_609_740_800;
    let max = 67_768_036_191_676_799;
    for n in [
        min,
        min + 1,
        -62_167_219_200,
        -1,
        0,
        2_147_483_648,
        max - 1,
        max,
    ] {
        assert_eq!(to_utc(&from_utc(n).unwrap()), Ok(n));
    }
    for n in [i64::MIN, min - 1, max + 1, i64::MAX] {
        assert_eq!(from_utc(n), Err(CivilError::DateRange));
    }
    for (year, bound) in [
        (MIN_YEAR - 1, true),
        (MIN_YEAR, false),
        (MAX_YEAR, false),
        (MAX_YEAR + 1, true),
        (i64::MIN, true),
        (i64::MAX, true),
    ] {
        let fields = TimeFields {
            year: Field::Integer(year),
            hour: Field::Integer(0),
            ..table(7)
        };
        let result = normalize_time(fields, utc_inverse, |_| Ok(UTC));
        if bound {
            assert_eq!(result, Err(CivilError::OutOfBound("year")));
        } else {
            assert_eq!(result.unwrap().civil.year, year);
        }
    }
    let f = TimeFields {
        year: Field::Integer(1969),
        month: Field::Integer(12),
        day: Field::Integer(31),
        hour: Field::Integer(23),
        min: Field::Integer(59),
        sec: Field::Integer(59),
        isdst: Some(false),
    };
    assert_eq!(
        normalize_time(f, utc_inverse, |_| Ok(UTC)),
        Err(CivilError::TimeRange {
            normalized: Some(from_utc(-1).unwrap())
        })
    );
    for (name, delta) in [
        ("month", 1),
        ("day", 0),
        ("hour", 0),
        ("min", 0),
        ("sec", 0),
    ] {
        for n in [i32::MIN as i64 + delta - 1, i32::MAX as i64 + delta + 1] {
            let mut f = table(7);
            let slot = match name {
                "month" => &mut f.month,
                "day" => &mut f.day,
                "hour" => &mut f.hour,
                "min" => &mut f.min,
                _ => &mut f.sec,
            };
            *slot = Field::Integer(n);
            assert_eq!(
                normalize_time(f, utc_inverse, |_| Ok(UTC)),
                Err(CivilError::OutOfBound(name))
            );
        }
    }
    let mut c = from_utc(0).unwrap();
    c.month = 2;
    c.day = 30;
    assert_eq!(to_utc(&c), Err(CivilError::DateRange));
    assert_eq!(
        local_civil(i64::MAX, LocalOffset { seconds: 1, ..UTC }),
        Err(CivilError::DateRange)
    );
    assert_eq!(
        difftime(i64::MAX, i64::MIN),
        18_446_744_073_709_551_615u128 as f64
    );
}

#[test]
fn bytes_modifiers_and_oracle_boundaries() {
    assert_eq!(
        date(b"!*t\0ignored", 0, |_| panic!()).unwrap(),
        DateOutput::Table(from_utc(0).unwrap())
    );
    assert_eq!(
        date(b"!\xff\0%Y", 0, |_| panic!()).unwrap(),
        DateOutput::Bytes(b"\xff\x001970".to_vec())
    );
    for format in [
        b"%Q suffix".as_slice(),
        b"%EQ",
        b"%OZ",
        b"%s",
        b"%P",
        b"%k",
        b"%0Y",
        b"%E",
        b"%",
    ] {
        let suffix = format[1..].to_vec();
        assert_eq!(
            date(format, 0, |_| Ok(UTC)),
            Err(CivilError::InvalidConversion(suffix))
        );
    }
    assert_eq!(
        date(b"%Q\0tail", 0, |_| Ok(UTC)),
        Err(CivilError::InvalidConversion(b"Q".to_vec()))
    );
    let mut called = false;
    let f = TimeFields {
        year: Field::Missing,
        ..table(7)
    };
    assert_eq!(
        normalize_time(
            f,
            |_, _| {
                called = true;
                Ok(0)
            },
            |_| Ok(UTC)
        ),
        Err(CivilError::Missing("year"))
    );
    assert!(!called);
    let no_hint = TimeFields {
        isdst: None,
        ..table(7)
    };
    normalize_time(
        no_hint,
        |wall, hint| {
            assert_eq!(hint, None);
            Ok(wall)
        },
        |_| Ok(UTC),
    )
    .unwrap();
    for (n, expected) in [
        (1583650799, false),
        (1583650800, true),
        (1604210399, true),
        (1604210400, false),
        (954658799, false),
        (954658800, true),
        (972799199, true),
        (972799200, false),
    ] {
        assert_eq!(new_york(n).unwrap().isdst, expected, "{n}");
    }
}
