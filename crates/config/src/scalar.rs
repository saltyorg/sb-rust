//! Plain scalars resolved as sb-go's YAML library (yaml.v3) resolves them
//! (config spec, "Schema rules": sb follows yaml.v3's result where saphyr
//! would differ). saphyr's loader keeps each scalar's source text, style
//! and tag (`early_parse(false)`); this is yaml.v3's `resolve` (go.yaml.in
//! yaml v3.0.5, `resolve.go`, and `decode.go`'s quoted-string rule) over
//! them. Domain code: saphyr's own resolver is YAML 1.2's core schema (no
//! leading-zero octal, `0b`, underscores, u64 or timestamps), and jiff's
//! strptime differs from Go's `time.Parse` (it turns second 60 into 59,
//! skips leading spaces, reads at most 9 fraction digits and only after
//! `.`), nor is jiff in sb. One difference: `!!binary` text stays as
//! written (yaml.v3 decodes the base64; config spec, differences).

use std::sync::LazyLock;

use saphyr::{ScalarStyle, Tag};

/// A scalar's value as yaml.v3 decodes it into Go's `interface{}`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// yaml.v3's `int`, `int64` and `uint64`, together.
    Int(i128),
    Float(f64),
    Str(String),
    /// A date or date-time yaml.v3 decodes as Go's `time.Time` (kept as
    /// its text).
    Timestamp(String),
}

/// The core schema's tag prefix, which `!!` stands for.
const CORE: &str = "tag:yaml.org,2002:";

/// What yaml.v3 resolved a scalar to, with the tag it names that by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolved {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Timestamp,
}

impl Resolved {
    fn tag(self) -> &'static str {
        match self {
            Self::Null => "!!null",
            Self::Bool => "!!bool",
            Self::Int => "!!int",
            Self::Float => "!!float",
            Self::Str => "!!str",
            Self::Timestamp => "!!timestamp",
        }
    }
}

/// A tag in yaml.v3's short form (`!!int`), or the tag as written: the
/// core prefix shortened whether it came as `!!` or verbatim
/// (`!<tag:yaml.org,2002:int>`), as Go's `shortTag`.
fn short_tag(tag: &Tag) -> String {
    let full = format!("{}{}", tag.handle, tag.suffix);
    match full.strip_prefix(CORE) {
        Some(rest) => format!("!!{rest}"),
        None => full,
    }
}

/// `text`, written in `style` with `tag`, as yaml.v3 resolves it, or
/// yaml.v3's error for a tag the text doesn't fit.
pub fn resolve(text: &str, style: ScalarStyle, tag: Option<&Tag>) -> Result<Value, String> {
    let mut tag = tag.map(short_tag).unwrap_or_default();
    // decode.go: an untagged (or `!`) quoted, literal or folded scalar is
    // a string, as is anything tagged `!!str`.
    let styled = !matches!(style, ScalarStyle::Plain);
    if tag == "!!str" || (styled && (tag.is_empty() || tag == "!")) {
        return Ok(Value::Str(text.to_string()));
    }
    // A plain scalar's non-specific `!` resolves as if untagged (yaml.v3
    // decodes `! 12` as 12).
    if tag == "!" {
        tag.clear();
    }
    // resolve.go: tags it can't resolve leave the text a string.
    if !matches!(
        tag.as_str(),
        "" | "!!bool" | "!!int" | "!!float" | "!!null" | "!!timestamp"
    ) {
        return Ok(Value::Str(text.to_string()));
    }
    let (resolved, value) = untagged(text, &tag);
    if tag.is_empty() || tag == resolved.tag() {
        return Ok(value);
    }
    // A `!!float` takes yaml.v3's int and int64, as a float (not a
    // uint64, beyond i64).
    if let ("!!float", Value::Int(int)) = (tag.as_str(), &value)
        && i64::try_from(*int).is_ok()
    {
        return Ok(Value::Float(*int as f64));
    }
    Err(format!(
        "cannot decode {} `{text}` as a {tag}",
        resolved.tag()
    ))
}

/// resolve.go's resolution of `text` by its first character, as for a
/// plain scalar; `tag` only decides whether timestamps are tried.
fn untagged(text: &str, tag: &str) -> (Resolved, Value) {
    let string = || (Resolved::Str, Value::Str(text.to_string()));
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return (Resolved::Null, Value::Null),
        "true" | "True" | "TRUE" => return (Resolved::Bool, Value::Bool(true)),
        "false" | "False" | "FALSE" => return (Resolved::Bool, Value::Bool(false)),
        ".nan" | ".NaN" | ".NAN" => return (Resolved::Float, Value::Float(f64::NAN)),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return (Resolved::Float, Value::Float(f64::INFINITY));
        }
        "-.inf" | "-.Inf" | "-.INF" => return (Resolved::Float, Value::Float(f64::NEG_INFINITY)),
        _ => {}
    }
    match text.bytes().next() {
        Some(b'.') => {
            float_after_dot(text).map_or_else(string, |f| (Resolved::Float, Value::Float(f)))
        }
        Some(b'+' | b'-' | b'0'..=b'9') => {
            if (tag.is_empty() || tag == "!!timestamp") && is_timestamp(text) {
                return (Resolved::Timestamp, Value::Timestamp(text.to_string()));
            }
            let plain = text.replace('_', "");
            if let Some(int) = go_parse_int(&plain, true) {
                return (Resolved::Int, Value::Int(int));
            }
            if let Some(int) = go_parse_int(&plain, false) {
                return (Resolved::Int, Value::Int(int));
            }
            if let Some(float) = yaml_style_float(&plain) {
                return (Resolved::Float, Value::Float(float));
            }
            // resolve.go's later `0b`/`0o` branches only repeat what
            // `ParseInt(_, 0, 64)` already accepted.
            string()
        }
        _ => string(),
    }
}

/// Go's `strconv.ParseInt(s, 0, 64)` (`signed`) or `ParseUint(s, 0, 64)`:
/// an optional sign (signed only), then `0x`, `0o` or `0b` in either case,
/// a leading `0` for octal, or decimal; within i64 or u64.
fn go_parse_int(s: &str, signed: bool) -> Option<i128> {
    let (negative, rest) = if signed {
        match s.as_bytes().first()? {
            b'+' => (false, &s[1..]),
            b'-' => (true, &s[1..]),
            _ => (false, s),
        }
    } else {
        (false, s)
    };
    let lower = rest.to_ascii_lowercase();
    let (radix, digits) = if let Some(d) = lower.strip_prefix("0x") {
        (16, d)
    } else if let Some(d) = lower.strip_prefix("0o") {
        (8, d)
    } else if let Some(d) = lower.strip_prefix("0b") {
        (2, d)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, &lower[1..])
    } else {
        (10, lower.as_str())
    };
    // `from_str_radix` takes a sign and refuses nothing empty itself:
    // digits only, then it refuses an empty string.
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let magnitude = i128::from(u64::from_str_radix(digits, radix).ok()?);
    let value = if negative { -magnitude } else { magnitude };
    let range = if signed {
        i128::from(i64::MIN)..=i128::from(i64::MAX)
    } else {
        0..=i128::from(u64::MAX)
    };
    range.contains(&value).then_some(value)
}

/// resolve.go's `yamlStyleFloat`, the same pattern.
static YAML_STYLE_FLOAT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$")
        .expect("a valid pattern")
});

/// `yamlStyleFloat`, then Go's `ParseFloat`, which refuses a value past
/// f64's range.
fn yaml_style_float(s: &str) -> Option<f64> {
    if !YAML_STYLE_FLOAT.is_match(s) {
        return None;
    }
    s.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Go's `ParseFloat` on text starting with `.` (resolve.go's `.` branch,
/// which skips the pattern): underscores allowed only between digits (Go's
/// `underscoreOK`), then the digits; nothing else Go reads starts with a
/// dot.
fn float_after_dot(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let digit_at = |at: Option<usize>| at.and_then(|at| b.get(at)).is_some_and(u8::is_ascii_digit);
    let between_digits = b
        .iter()
        .enumerate()
        .all(|(at, &c)| c != b'_' || (digit_at(at.checked_sub(1)) && digit_at(Some(at + 1))));
    if !between_digits {
        return None;
    }
    yaml_style_float(&s.replace('_', ""))
}

/// resolve.go's `parseTimestamp`: four digits and `-`, then one of
/// `allowedTimestampFormats` as Go's `time.Parse` reads it.
fn is_timestamp(s: &str) -> bool {
    let Some(year) = s.get(..4).filter(|y| y.bytes().all(|b| b.is_ascii_digit())) else {
        return false;
    };
    let Some(rest) = s[4..].strip_prefix('-') else {
        return false;
    };
    let (date, time) = match rest.find(['T', 't', ' ']) {
        Some(at) => (&rest[..at], Some((rest.as_bytes()[at], &rest[at + 1..]))),
        None => (rest, None),
    };
    // Four ASCII digits always parse.
    if !valid_date(year.parse().unwrap_or_default(), date) {
        return false;
    }
    match time {
        None => true,
        // `2006-1-2 15:4:5.999999999`: no zone; Go's layout space takes a
        // run of spaces.
        Some((b' ', rest)) => valid_time(rest.trim_start_matches(' ')).is_some_and(str::is_empty),
        // `2006-1-2T15:4:5.999999999Z07:00`: a zone, `Z` or `±hh:mm`.
        Some((_, rest)) => valid_time(rest).is_some_and(valid_zone),
    }
}

/// Go's `getnum(_, false)`: one or two digits, and what follows them.
fn one_or_two_digits(s: &str) -> Option<(u32, &str)> {
    let n = s.bytes().take(2).take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    Some((s[..n].parse().ok()?, &s[n..]))
}

/// `1-2` of `2006-1-2` in `year`: a month and a day of one or two digits,
/// the day within the month (leap years included).
fn valid_date(year: u32, s: &str) -> bool {
    let Some((month, rest)) = one_or_two_digits(s) else {
        return false;
    };
    let Some((day, rest)) = rest.strip_prefix('-').and_then(one_or_two_digits) else {
        return false;
    };
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    rest.is_empty() && (1..=days).contains(&day)
}

/// `15:4:5.999999999`: hour, minute and second of one or two digits in
/// range, then an optional fraction (Go reads any number of digits past
/// the layout's nine); what follows it.
fn valid_time(s: &str) -> Option<&str> {
    let (hour, rest) = one_or_two_digits(s)?;
    let (minute, rest) = one_or_two_digits(rest.strip_prefix(':')?)?;
    let (second, rest) = one_or_two_digits(rest.strip_prefix(':')?)?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    match rest.strip_prefix(['.', ',']) {
        Some(frac) => {
            let n = frac.bytes().take_while(u8::is_ascii_digit).count();
            (n > 0).then(|| &frac[n..])
        }
        None => Some(rest),
    }
}

/// `Z07:00`: `Z`, or a sign and `hh:mm` with the hour up to 24 and the
/// minute up to 60, as Go's `time.Parse` checks them.
fn valid_zone(s: &str) -> bool {
    if s == "Z" {
        return true;
    }
    let b = s.as_bytes();
    let two = |at: usize| s.get(at..at + 2).and_then(|t| t.parse::<u32>().ok());
    b.len() == 6
        && matches!(b[0], b'+' | b'-')
        && b[3] == b':'
        && b[1..3].iter().chain(&b[4..6]).all(u8::is_ascii_digit)
        && two(1).is_some_and(|hour| hour <= 24)
        && two(4).is_some_and(|minute| minute <= 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use saphyr::{MarkedYamlOwned, YamlDataOwned};

    /// The value of `v` in `v: <source>` as saphyr's loader keeps it,
    /// resolved.
    fn value(source: &str) -> Result<Value, String> {
        let mut loader = saphyr::YamlLoader::<MarkedYamlOwned>::default();
        loader.early_parse(false);
        let text = format!("v: {source}\n");
        saphyr_parser::Parser::new_from_str(&text)
            .load(&mut loader, false)
            .expect("parses");
        let doc = loader.into_documents().remove(0);
        let YamlDataOwned::Mapping(map) = doc.data else {
            panic!("not a mapping");
        };
        let node = map.values().next().expect("v");
        match &node.data {
            YamlDataOwned::Representation(text, style, tag) => resolve(text, *style, tag.as_ref()),
            other => panic!("not a scalar: {other:?}"),
        }
    }

    fn ok(source: &str) -> Value {
        value(source).unwrap_or_else(|err| panic!("{source}: {err}"))
    }

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    #[test]
    fn nulls() {
        for source in ["", "~", "null", "Null", "NULL"] {
            assert_eq!(ok(source), Value::Null, "{source:?}");
        }
        assert_eq!(ok("nULL"), s("nULL"));
    }

    /// YAML 1.2 booleans only: yes/no/on/off/y/n stay strings in yaml.v3.
    #[test]
    fn booleans() {
        for source in ["true", "True", "TRUE"] {
            assert_eq!(ok(source), Value::Bool(true), "{source}");
        }
        for source in ["false", "False", "FALSE"] {
            assert_eq!(ok(source), Value::Bool(false), "{source}");
        }
        for source in ["tRUE", "yes", "Yes", "no", "on", "off", "y", "n"] {
            assert_eq!(ok(source), s(source), "{source}");
        }
    }

    #[test]
    fn special_floats_and_merge() {
        assert!(matches!(ok(".nan"), Value::Float(f) if f.is_nan()));
        assert_eq!(ok(".inf"), Value::Float(f64::INFINITY));
        assert_eq!(ok("+.Inf"), Value::Float(f64::INFINITY));
        assert_eq!(ok("-.INF"), Value::Float(f64::NEG_INFINITY));
        assert_eq!(ok("inf"), s("inf"));
        assert_eq!(ok("<<"), s("<<"));
    }

    /// Go's `ParseInt(_, 0, 64)` after removing underscores: a leading 0
    /// is octal, and the 0x/0o/0b prefixes work in either case.
    #[test]
    fn integers() {
        let cases: [(&str, i128); 21] = [
            ("0", 0),
            ("12", 12),
            ("+12", 12),
            ("-12", -12),
            ("017", 15),
            ("0_17", 15),
            ("0o17", 15),
            ("0O17", 15),
            ("0x1F", 31),
            ("0X1f", 31),
            ("-0x10", -16),
            ("+0x10", 16),
            ("+0b1", 1),
            ("0B11", 3),
            ("+1_0", 10),
            ("0b101", 5),
            ("-0b101", -5),
            ("1_000", 1000),
            // Every underscore goes, wherever it is.
            ("1__", 1),
            ("9223372036854775807", 9_223_372_036_854_775_807),
            ("-9223372036854775808", -9_223_372_036_854_775_808),
        ];
        for (source, want) in cases {
            assert_eq!(ok(source), Value::Int(want), "{source}");
        }
        // Past i64: Go's ParseUint, up to u64's maximum.
        assert_eq!(
            ok("9223372036854775808"),
            Value::Int(9_223_372_036_854_775_808)
        );
        assert_eq!(
            ok("18446744073709551615"),
            Value::Int(18_446_744_073_709_551_615)
        );
    }

    #[test]
    fn floats() {
        let cases: [(&str, f64); 18] = [
            ("1.5", 1.5),
            ("1.", 1.0),
            (".5", 0.5),
            ("+.5", 0.5),
            ("-1.5e-3", -0.0015),
            ("1e3", 1000.0),
            ("1E3", 1000.0),
            ("1e+3", 1000.0),
            ("-.5", -0.5),
            (".5e3", 500.0),
            ("1_000.5", 1000.5),
            // An invalid octal falls through to the float pattern.
            ("08", 8.0),
            ("09.5", 9.5),
            // The `.` branch is Go's ParseFloat: underscores between digits.
            (".5_5", 0.55),
            (".1e1_0", 1e9),
            // Past u64, and below i64: floats.
            ("18446744073709551616", 18_446_744_073_709_551_616.0),
            ("-9223372036854775809", -9_223_372_036_854_775_809.0),
            // Go's ParseUint takes no sign: past i64 with a `+`, a float.
            ("+9223372036854775808", 9_223_372_036_854_775_808.0),
        ];
        for (source, want) in cases {
            assert_eq!(ok(source), Value::Float(want), "{source}");
        }
    }

    #[test]
    fn what_stays_a_string() {
        for source in [
            "abc", "1e400", ".e3", "._5", ".5_", ".5__0", "1.0.0", "0x", "+", "12:30", "0b", "-0o",
            "0xG",
        ] {
            assert_eq!(ok(source), s(source), "{source}");
        }
    }

    /// (Every expectation in this module was checked against yaml.v3
    /// v3.0.5 itself, decoding into `interface{}` as sb-go does.)
    ///
    /// yaml.v3 decodes these as Go's `time.Time`: a four-digit year, then
    /// Go's layouts `2006-1-2`, `2006-1-2 15:4:5.999999999` and
    /// `2006-1-2T15:4:5.999999999Z07:00` (either case of T), dates checked.
    #[test]
    fn timestamps() {
        for source in [
            "2001-12-14",
            "2001-1-2",
            "2000-02-29",
            "2001-12-14 21:59:43",
            "2001-12-14 21:59:43.10",
            "2001-12-14 1:2:3",
            "2001-12-14T21:59:43Z",
            "2001-12-14t21:59:43.123456789Z",
            "2001-12-14 21:59:43.1234567890",
            "2001-12-14T21:59:43+01:00",
            "2001-12-14T21:59:43-05:30",
            "2001-12-14T1:2:3Z",
            "2001-12-14T21:59:43.5+01:00",
            "0001-01-01",
            "2001-1-02",
            "9999-12-31T23:59:59Z",
            "2001-04-30",
            "2001-02-28",
            // Go: a comma before the fraction, a run of spaces for the
            // layout's one, and a zone up to +24:00 and :60.
            "2001-12-14 21:59:43,5",
            "2001-12-14T21:59:43,5Z",
            "2001-12-14  21:59:43",
            "2001-12-14T21:59:43+24:00",
            "2001-12-14T21:59:43+01:60",
        ] {
            assert_eq!(ok(source), Value::Timestamp(source.to_string()), "{source}");
        }
        for source in [
            "2001-13-01",
            "2001-02-30",
            "1900-02-29",
            "2001-12-14T21:59:43",
            "2001-12-14T25:00:00Z",
            "2001-12-14 21:60:00",
            "2001-12-14 21:59:60",
            "2001-12-14T21:59:43+0100",
            "2001-12-14 21:59:43.10 -5",
            "2001-012-14",
            "201-01-01",
            "2001-12-",
            "2001-12-14x",
            "2001-12-14 21:59:43.",
            "2001-12-14 21:59:43.5Z",
            "2001-12-14 21:59",
            "+2001-01-01",
            "2001-12-14T24:00:00Z",
            "2001-04-31",
            "2001-ab-01",
            "2001-01-ab",
            "2001x01-01",
            "2001-12-14T21:59:43+25:00",
            "2001-12-14T21:59:43+23:61",
        ] {
            assert!(!matches!(ok(source), Value::Timestamp(_)), "{source}");
        }
    }

    /// Quoted, literal and folded scalars are strings unless tagged.
    #[test]
    fn styled_scalars_are_strings() {
        assert_eq!(ok("'017'"), s("017"));
        assert_eq!(ok("\"true\""), s("true"));
        assert_eq!(ok("|\n  12\n"), s("12\n"));
        assert_eq!(ok(">-\n  null\n"), s("null"));
    }

    #[test]
    fn tags() {
        assert_eq!(ok("!!str 017"), s("017"));
        assert_eq!(ok("!!int 017"), Value::Int(15));
        assert_eq!(ok("!!int \"5\""), Value::Int(5));
        assert_eq!(ok("!!float 017"), Value::Float(15.0));
        assert_eq!(ok("!!null ''"), Value::Null);
        assert_eq!(
            ok("!!timestamp 2001-01-01"),
            Value::Timestamp("2001-01-01".into())
        );
        // Tags yaml.v3 doesn't resolve leave the text a string.
        assert_eq!(ok("!vault abc"), s("abc"));
        assert_eq!(ok("! 12"), Value::Int(12));
        assert_eq!(ok("! '12'"), s("12"));
        assert_eq!(ok("!!seq x"), s("x"));
        assert_eq!(
            value("!!int abc"),
            Err("cannot decode !!str `abc` as a !!int".into())
        );
        assert_eq!(
            value("!!bool yes"),
            Err("cannot decode !!str `yes` as a !!bool".into())
        );
        assert_eq!(
            value("!!int 1.5"),
            Err("cannot decode !!float `1.5` as a !!int".into())
        );
        // `!!float` takes an i64 (yaml.v3's int and int64), not a uint64.
        assert_eq!(
            ok("!!float -9223372036854775808"),
            Value::Float(-9_223_372_036_854_775_808.0)
        );
        assert_eq!(
            value("!!float 9223372036854775808"),
            Err("cannot decode !!int `9223372036854775808` as a !!float".into())
        );
        assert_eq!(ok("!!float .5_5"), Value::Float(0.55));
        assert_eq!(
            value("!!int true"),
            Err("cannot decode !!bool `true` as a !!int".into())
        );
        // A verbatim core tag is the same tag.
        assert_eq!(ok("!<tag:yaml.org,2002:int> 7"), Value::Int(7));
        assert_eq!(ok("!<tag:yaml.org,2002:str> 7"), s("7"));
    }
}
