//! Snowflake date/time parts and format strings.

/// A canonical date or time part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
    DayOfWeek,
    DayOfWeekIso,
    DayOfYear,
    WeekIso,
    YearOfWeek,
    YearOfWeekIso,
    Epoch,
    EpochMillisecond,
    EpochMicrosecond,
    EpochNanosecond,
    TimezoneHour,
    TimezoneMinute,
}

impl Part {
    /// Resolves the part names and abbreviations Snowflake accepts.
    pub fn parse(name: &str) -> Option<Self> {
        use Part::*;
        Some(match name.to_ascii_lowercase().as_str() {
            "year" | "y" | "yy" | "yyy" | "yyyy" | "yr" | "years" | "yrs" => Year,
            "quarter" | "q" | "qtr" | "qtrs" | "quarters" => Quarter,
            "month" | "mm" | "mon" | "mons" | "months" => Month,
            "week" | "w" | "wk" | "weekofyear" | "woy" | "wy" => Week,
            "day" | "d" | "dd" | "days" | "dayofmonth" => Day,
            "hour" | "h" | "hh" | "hr" | "hours" | "hrs" => Hour,
            "minute" | "m" | "mi" | "min" | "minutes" | "mins" => Minute,
            "second" | "s" | "sec" | "seconds" | "secs" => Second,
            "millisecond" | "ms" | "msec" | "milliseconds" => Millisecond,
            "microsecond" | "us" | "usec" | "microseconds" => Microsecond,
            "nanosecond" | "ns" | "nsec" | "nanosec" | "nsecond" | "nanoseconds" | "nanosecs" | "nseconds" => {
                Nanosecond
            }
            "dayofweek" | "weekday" | "dow" | "dw" => DayOfWeek,
            "dayofweekiso" | "weekday_iso" | "dow_iso" | "dw_iso" => DayOfWeekIso,
            "dayofyear" | "yearday" | "doy" | "dy" => DayOfYear,
            "weekiso" | "week_iso" | "weekofyeariso" | "weekofyear_iso" => WeekIso,
            "yearofweek" => YearOfWeek,
            "yearofweekiso" => YearOfWeekIso,
            "epoch_second" | "epoch" | "epoch_seconds" => Epoch,
            "epoch_millisecond" | "epoch_milliseconds" => EpochMillisecond,
            "epoch_microsecond" | "epoch_microseconds" => EpochMicrosecond,
            "epoch_nanosecond" | "epoch_nanoseconds" => EpochNanosecond,
            "timezone_hour" | "tzh" => TimezoneHour,
            "timezone_minute" | "tzm" => TimezoneMinute,
            _ => return None,
        })
    }

    /// The part name DuckDB uses in `date_part`, `date_trunc` and `date_diff`.
    pub fn duckdb_name(self) -> &'static str {
        use Part::*;
        match self {
            Year => "year",
            Quarter => "quarter",
            Month => "month",
            Week => "week",
            Day => "day",
            Hour => "hour",
            Minute => "minute",
            Second => "second",
            Millisecond => "millisecond",
            Microsecond => "microsecond",
            Nanosecond => "nanosecond",
            DayOfWeek => "dayofweek",
            DayOfWeekIso => "isodow",
            DayOfYear => "dayofyear",
            WeekIso => "week",
            YearOfWeek | YearOfWeekIso => "isoyear",
            Epoch => "epoch",
            EpochMillisecond => "epoch_ms",
            EpochMicrosecond => "epoch_us",
            EpochNanosecond => "epoch_ns",
            TimezoneHour => "timezone_hour",
            TimezoneMinute => "timezone_minute",
        }
    }

    /// True for parts at day granularity or coarser, where DATE inputs stay DATEs.
    pub fn is_date_level(self) -> bool {
        matches!(self, Part::Year | Part::Quarter | Part::Month | Part::Week | Part::Day)
    }

    /// The DuckDB function that builds an interval of `n` of this part, and a multiplier.
    pub fn interval_fn(self) -> Option<(&'static str, i64)> {
        use Part::*;
        Some(match self {
            Year => ("to_years", 1),
            Quarter => ("to_months", 3),
            Month => ("to_months", 1),
            Week => ("to_weeks", 1),
            Day => ("to_days", 1),
            Hour => ("to_hours", 1),
            Minute => ("to_minutes", 1),
            Second => ("to_seconds", 1),
            Millisecond => ("to_milliseconds", 1),
            Microsecond => ("to_microseconds", 1),
            _ => return None,
        })
    }
}

/// Converts a Snowflake date/time format (e.g. `YYYY-MM-DD HH24:MI:SS.FF3`)
/// into a strftime/strptime format for DuckDB. `AUTO` returns `None`.
pub fn convert_format(format: &str) -> Option<String> {
    if format.eq_ignore_ascii_case("auto") {
        return None;
    }
    // Longest tokens first so e.g. MMMM wins over MM and HH24 over HH.
    const TOKENS: &[(&str, &str)] = &[
        ("YYYY", "%Y"),
        ("MMMM", "%B"),
        ("HH24", "%H"),
        ("HH12", "%I"),
        ("TZH:TZM", "%z"),
        ("TZHTZM", "%z"),
        ("FF9", "%n"),
        ("FF8", "%n"),
        ("FF7", "%n"),
        ("FF6", "%f"),
        ("FF5", "%f"),
        ("FF4", "%f"),
        ("FF3", "%g"),
        ("FF2", "%g"),
        ("FF1", "%g"),
        ("FF0", ""),
        ("MON", "%b"),
        ("DY", "%a"),
        ("YY", "%y"),
        ("MM", "%m"),
        ("DD", "%d"),
        ("HH", "%I"),
        ("MI", "%M"),
        ("SS", "%S"),
        ("FF", "%n"),
        ("AM", "%p"),
        ("PM", "%p"),
        ("TZH", "%z"),
    ];
    let mut out = String::new();
    let mut rest = format;
    'outer: while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('"') {
            // Quoted literal text.
            let end = stripped.find('"').unwrap_or(stripped.len());
            out.push_str(&stripped[..end].replace('%', "%%"));
            rest = stripped.get(end + 1..).unwrap_or("");
            continue;
        }
        for (token, replacement) in TOKENS {
            if rest.len() >= token.len() && rest[..token.len()].eq_ignore_ascii_case(token) {
                out.push_str(replacement);
                rest = &rest[token.len()..];
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap();
        if c == '%' {
            out.push('%');
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_formats() {
        assert_eq!(convert_format("YYYY-MM-DD").unwrap(), "%Y-%m-%d");
        assert_eq!(convert_format("YYYY-MM-DD HH24:MI:SS.FF3").unwrap(), "%Y-%m-%d %H:%M:%S.%g");
        assert_eq!(convert_format("DD-MON-YYYY").unwrap(), "%d-%b-%Y");
        assert_eq!(convert_format("MM/DD/YY HH12:MI AM").unwrap(), "%m/%d/%y %I:%M %p");
        assert_eq!(convert_format("YYYY\"T\"HH24").unwrap(), "%YT%H");
        assert_eq!(convert_format("auto"), None);
    }

    #[test]
    fn parses_part_aliases() {
        assert_eq!(Part::parse("DD"), Some(Part::Day));
        assert_eq!(Part::parse("mon"), Some(Part::Month));
        assert_eq!(Part::parse("hours"), Some(Part::Hour));
        assert_eq!(Part::parse("nope"), None);
    }
}
