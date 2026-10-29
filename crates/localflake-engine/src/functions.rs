//! Scalar functions implemented in Rust and registered with DuckDB.
//!
//! These cover Snowflake behaviour where the result type depends on the
//! argument type. Each signature becomes a DuckDB overload, so DuckDB picks the
//! right one when it binds the query.

use std::error::Error;
use std::sync::Arc;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use duckdb::Connection;
use duckdb::arrow::array::{
    Array, ArrayRef, AsArray, Date32Array, Decimal128Array, Float64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use duckdb::arrow::compute::cast;
use duckdb::arrow::datatypes::{
    DataType, Date32Type, Decimal128Type, Float64Type, Int64Type, TimeUnit, TimestampMicrosecondType,
};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const MICROS: i64 = 1_000_000;
const UNIX_EPOCH_DAYS_FROM_CE: i32 = 719_163;

pub fn register(conn: &Connection) -> duckdb::Result<()> {
    conn.register_scalar_function::<Divide>("sf_div")?;
    conn.register_scalar_function::<CastLike>("sf_cast_like")?;
    conn.register_scalar_function::<ToTimestamp>("sf_to_timestamp")?;
    conn.register_scalar_function::<ToTimestampTz>("sf_to_timestamp_tz")?;
    conn.register_scalar_function::<ToDate>("sf_to_date")?;
    conn.register_scalar_function::<Soundex>("sf_soundex")?;
    conn.register_scalar_function::<SoundexP123>("sf_soundex_p123")?;
    conn.register_scalar_function::<Wait>("sf_wait")?;
    conn.register_scalar_function::<crate::sketch::HllState>("sf_hll_state")?;
    conn.register_scalar_function::<crate::sketch::HllHashes>("sf_hll_hashes")?;
    conn.register_scalar_function::<crate::sketch::BitmapState>("sf_bitmap_state")?;
    conn.register_scalar_function::<crate::sketch::BitmapPositions>("sf_bitmap_positions")?;
    conn.register_scalar_function::<crate::sketch::MinHash>("sf_minhash")?;
    conn.register_scalar_function::<Sha2>("sf_sha2")?;
    conn.register_scalar_function::<InitCap>("sf_initcap")?;
    conn.register_scalar_function::<ParseUrl>("sf_parse_url")?;
    conn.register_scalar_function::<ConvertTimezone>("sf_convert_timezone")?;
    conn.register_scalar_function::<FlattenRecursive>("sf_flatten_recursive")?;
    conn.register_scalar_function::<FormatNumber>("sf_format_number")?;
    conn.register_scalar_function::<JsonNulls>("sf_json_nulls")?;
    conn.register_scalar_function::<JsonRender>("sf_json_render")?;
    conn.register_scalar_function::<VariantLabel>("sf_variant_label")?;
    conn.register_scalar_function::<Compress>("sf_compress")?;
    conn.register_scalar_function::<HasTzOffset>("sf_has_tz_offset")?;
    conn.register_scalar_function::<TzOffsetText>("sf_tz_offset_text")?;
    conn.register_scalar_function::<TzInZone>("sf_tz_in_zone")?;
    conn.register_scalar_function::<TzRewrap>("sf_tz_rewrap")?;
    conn.register_scalar_function::<TzInstant>("sf_tz_instant")?;
    conn.register_scalar_function::<Cbrt>("sf_cbrt")?;
    conn.register_scalar_function::<RegexpReplace>("sf_regexp_replace")?;
    conn.register_scalar_function::<crate::match_recognize::MatchRecognize>("sf_match_recognize")?;
    conn.register_scalar_function::<Decompress>("sf_decompress")?;
    conn.register_scalar_function::<NumberText>("sf_number_text")?;
    Ok(())
}

fn ts() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, None)
}

fn ts_tz() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
}

fn decimal(scale: i8) -> DataType {
    DataType::Decimal128(38, scale)
}

/// `a / b` with Snowflake semantics: integer operands give `NUMBER(38,6)`
/// (rounded half away from zero), and division by zero is an error.
struct Divide;

impl VArrowScalar for Divide {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (a, b) = (input.column(0), input.column(1));
        if a.data_type() == &DataType::Float64 {
            let (a, b) = (a.as_primitive::<Float64Type>(), b.as_primitive::<Float64Type>());
            let out: Float64Array = a
                .iter()
                .zip(b.iter())
                .map(|(a, b)| match (a, b) {
                    (Some(_), Some(0.0)) => Err("Division by zero".into()),
                    (Some(a), Some(b)) => Ok(Some(a / b)),
                    _ => Ok(None),
                })
                .collect::<Result<_>>()?;
            return Ok(Arc::new(out));
        }
        let ints = |array: &ArrayRef| -> Result<Vec<Option<i128>>> {
            Ok(match array.data_type() {
                DataType::Int64 => array.as_primitive::<Int64Type>().iter().map(|v| v.map(i128::from)).collect(),
                _ => array.as_primitive::<Decimal128Type>().iter().collect(),
            })
        };
        let out: Decimal128Array = ints(a)?
            .into_iter()
            .zip(ints(b)?)
            .map(|(a, b)| match (a, b) {
                (Some(_), Some(0)) => Err("Division by zero".into()),
                (Some(a), Some(b)) => {
                    let scaled = a.checked_mul(2_000_000).ok_or("Number out of representable range")?;
                    let quotient = scaled / b;
                    // Round half away from zero.
                    let rounded = if quotient >= 0 { (quotient + 1) / 2 } else { (quotient - 1) / 2 };
                    Ok(Some(rounded))
                }
                _ => Ok(None),
            })
            .collect::<Result<Decimal128Array>>()?
            .with_precision_and_scale(38, 6)?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![
            ArrowFunctionSignature::exact(vec![DataType::Int64, DataType::Int64], decimal(6)),
            ArrowFunctionSignature::exact(vec![decimal(0), decimal(0)], decimal(6)),
            ArrowFunctionSignature::exact(vec![DataType::Float64, DataType::Float64], DataType::Float64),
        ]
    }
}

/// `sf_cast_like(value, template)` casts `value` to the type of `template`.
/// Used to keep DATE inputs as DATEs after interval arithmetic or truncation.
struct CastLike;

impl VArrowScalar for CastLike {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (value, template) = (input.column(0), input.column(1));
        Ok(cast(value, template.data_type())?)
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![
            ArrowFunctionSignature::exact(vec![ts(), DataType::Date32], DataType::Date32),
            ArrowFunctionSignature::exact(vec![DataType::Date32, DataType::Date32], DataType::Date32),
            ArrowFunctionSignature::exact(vec![ts(), ts()], ts()),
            ArrowFunctionSignature::exact(vec![ts_tz(), ts_tz()], ts_tz()),
        ]
    }
}

/// Conversions to TIMESTAMP_NTZ (`TO_TIMESTAMP_NTZ`, `::TIMESTAMP_NTZ`).
struct ToTimestamp;

impl VArrowScalar for ToTimestamp {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let micros = to_micros(input.column(0), false)?;
        Ok(Arc::new(match session_zone(&input)? {
            // An instant (TIMESTAMP_LTZ) becomes the wall clock time in the session's zone.
            Some(zone) if is_instant(input.column(0)) => localize(&micros, zone)?,
            _ => micros,
        }))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        let mut sigs = timestamp_signatures(ts());
        sigs.extend(zoned_signatures(ts()));
        sigs
    }
}

/// `(input, session time zone)` overloads of the conversion functions.
fn zoned_signatures(output: DataType) -> Vec<ArrowFunctionSignature> {
    [DataType::Utf8, DataType::Int64, DataType::Float64, DataType::Date32, ts(), ts_tz()]
        .into_iter()
        .map(|input| ArrowFunctionSignature::exact(vec![input, DataType::Utf8], output.clone()))
        .collect()
}

fn is_instant(array: &ArrayRef) -> bool {
    matches!(array.data_type(), DataType::Timestamp(_, Some(_)))
}

/// The session time zone passed as the second argument, if any.
fn session_zone(input: &RecordBatch) -> Result<Option<chrono_tz::Tz>> {
    if input.num_columns() < 2 || input.num_rows() == 0 {
        return Ok(None);
    }
    let zones = input.column(1).as_string::<i32>();
    if zones.is_null(0) {
        return Ok(None);
    }
    let name = zones.value(0);
    Ok(Some(name.parse().map_err(|_| format!("Unknown timezone: {name}"))?))
}

/// UTC instants (microseconds) as wall clock microseconds in `zone`.
fn localize(micros: &TimestampMicrosecondArray, zone: chrono_tz::Tz) -> Result<TimestampMicrosecondArray> {
    use chrono::Offset;
    use chrono::TimeZone;
    Ok(micros
        .iter()
        .map(|m| {
            m.map(|m| {
                let instant = DateTime::from_timestamp_micros(m).unwrap_or_default();
                let offset = zone.offset_from_utc_datetime(&instant.naive_utc()).fix().local_minus_utc();
                m + i64::from(offset) * MICROS
            })
        })
        .collect())
}

/// Conversions to TIMESTAMP_LTZ/TZ, which DuckDB stores as TIMESTAMPTZ (UTC instants).
struct ToTimestampTz;

impl VArrowScalar for ToTimestampTz {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        Ok(Arc::new(to_micros(input.column(0), true)?.with_timezone("UTC")))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        timestamp_signatures(ts_tz())
    }
}

fn timestamp_signatures(output: DataType) -> Vec<ArrowFunctionSignature> {
    [DataType::Utf8, DataType::Int64, DataType::Float64, DataType::Date32, ts(), ts_tz()]
        .into_iter()
        .map(|input| ArrowFunctionSignature::exact(vec![input], output.clone()))
        .collect()
}

fn to_micros(array: &ArrayRef, with_tz: bool) -> Result<TimestampMicrosecondArray> {
    Ok(match array.data_type() {
        DataType::Utf8 => array
            .as_string::<i32>()
            .iter()
            .map(|s| s.map(|s| parse_timestamp(s, with_tz)).transpose())
            .collect::<Result<_>>()?,
        DataType::Int64 => array.as_primitive::<Int64Type>().iter().map(|v| v.map(|s| s * MICROS)).collect(),
        DataType::Float64 => {
            array.as_primitive::<Float64Type>().iter().map(|v| v.map(|s| (s * MICROS as f64).round() as i64)).collect()
        }
        DataType::Date32 => {
            array.as_primitive::<Date32Type>().iter().map(|d| d.map(|d| i64::from(d) * 86_400 * MICROS)).collect()
        }
        DataType::Timestamp(_, _) => array.as_primitive::<TimestampMicrosecondType>().iter().collect(),
        other => return Err(format!("cannot convert {other} to a timestamp").into()),
    })
}

/// Conversions to DATE (`TO_DATE`, `::DATE`).
struct ToDate;

impl VArrowScalar for ToDate {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let array = input.column(0);
        let days = |micros: i64| micros.div_euclid(86_400 * MICROS) as i32;
        let out: Date32Array = match array.data_type() {
            DataType::Utf8 => {
                array.as_string::<i32>().iter().map(|s| s.map(parse_date).transpose()).collect::<Result<_>>()?
            }
            DataType::Int64 => {
                array.as_primitive::<Int64Type>().iter().map(|v| v.map(|s| days(epoch_micros(s)))).collect()
            }
            DataType::Date32 => array.as_primitive::<Date32Type>().clone(),
            DataType::Timestamp(_, tz) => {
                let micros = array.as_primitive::<TimestampMicrosecondType>().clone();
                let micros = match session_zone(&input)? {
                    Some(zone) if tz.is_some() => localize(&micros, zone)?,
                    _ => micros,
                };
                micros.iter().map(|v| v.map(days)).collect()
            }
            other => return Err(format!("cannot convert {other} to a date").into()),
        };
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        [DataType::Utf8, DataType::Int64, DataType::Date32, ts(), ts_tz()]
            .into_iter()
            .flat_map(|input| {
                [
                    ArrowFunctionSignature::exact(vec![input.clone()], DataType::Date32),
                    ArrowFunctionSignature::exact(vec![input, DataType::Utf8], DataType::Date32),
                ]
            })
            .collect()
    }
}

/// Snowflake's rule for integers given as strings: the magnitude decides
/// whether they are seconds, milliseconds, microseconds or nanoseconds.
fn epoch_micros(value: i64) -> i64 {
    match value.unsigned_abs() {
        v if v < 31_536_000_000 => value * MICROS,
        v if v < 31_536_000_000_000 => value * 1_000,
        v if v < 31_536_000_000_000_000 => value,
        _ => value / 1_000,
    }
}

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    "%Y/%m/%d %H:%M:%S%.f",
    "%d-%b-%Y %H:%M:%S%.f",
    "%m/%d/%Y %H:%M:%S%.f",
];

const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%Y/%m/%d", "%d-%b-%Y", "%m/%d/%Y", "%Y%m%d"];

const OFFSET_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%.f%:z",
    "%Y-%m-%dT%H:%M:%S%.f%:z",
    "%Y-%m-%d %H:%M:%S%.f %:z",
    "%Y-%m-%d %H:%M:%S%.f%z",
    "%Y-%m-%dT%H:%M:%S%.f%z",
    "%Y-%m-%d %H:%M:%S%.f %z",
    "%Y-%m-%d %H:%M%:z",
    "%Y-%m-%dT%H:%M%:z",
];

/// Parses a timestamp string the way Snowflake's AUTO input format does.
/// With an explicit UTC offset, NTZ keeps the wall-clock time and LTZ/TZ
/// convert to UTC.
/// Timestamp text with an explicit offset, as `(wall clock micros, offset seconds)`.
pub(crate) fn parse_with_offset(text: &str) -> Option<(i64, i32)> {
    let s = text.trim();
    let candidate = match s.strip_suffix('Z').or_else(|| s.strip_suffix('z')) {
        Some(rest) => format!("{rest}+00:00"),
        None => s.to_owned(),
    };
    OFFSET_FORMATS.iter().find_map(|format| {
        DateTime::parse_from_str(&candidate, format)
            .ok()
            .map(|dt| (dt.naive_local().and_utc().timestamp_micros(), dt.offset().local_minus_utc()))
    })
}

pub(crate) fn parse_timestamp(text: &str, with_tz: bool) -> Result<i64> {
    let s = text.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Ok(epoch_micros(n));
    }
    let s_z = s.strip_suffix('Z').or_else(|| s.strip_suffix('z'));
    let candidate = s_z.map(|s| format!("{s}+00:00")).unwrap_or_else(|| s.to_owned());
    for format in OFFSET_FORMATS {
        if let Ok(dt) = DateTime::parse_from_str(&candidate, format) {
            let naive = if with_tz { dt.naive_utc() } else { dt.naive_local() };
            return Ok(naive.and_utc().timestamp_micros());
        }
    }
    for format in DATETIME_FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, format) {
            return Ok(dt.and_utc().timestamp_micros());
        }
    }
    for format in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, format) {
            return Ok(d.and_time(NaiveTime::MIN).and_utc().timestamp_micros());
        }
    }
    Err(format!("Timestamp '{text}' is not recognized").into())
}

fn parse_date(text: &str) -> Result<i32> {
    let s = text.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Ok(epoch_micros(n).div_euclid(86_400 * MICROS) as i32);
    }
    for format in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, format) {
            return Ok(d.num_days_from_ce() - UNIX_EPOCH_DAYS_FROM_CE);
        }
    }
    match parse_timestamp(s, false) {
        Ok(micros) => Ok(micros.div_euclid(86_400 * MICROS) as i32),
        Err(_) => Err(format!("Date '{text}' is not recognized").into()),
    }
}

/// Applies `f` to each string of column 0 (and the optional string column 1).
fn map_strings(input: &RecordBatch, f: impl Fn(&str, Option<&str>) -> Result<Option<String>>) -> Result<ArrayRef> {
    let values = input.column(0).as_string::<i32>();
    let extra = (input.num_columns() > 1).then(|| input.column(1).as_string::<i32>());
    let out: StringArray = (0..values.len())
        .map(|i| {
            if values.is_null(i) || extra.is_some_and(|e| e.is_null(i)) {
                return Ok(None);
            }
            f(values.value(i), extra.map(|e| e.value(i)))
        })
        .collect::<Result<_>>()?;
    Ok(Arc::new(out))
}

fn strings(arity: usize) -> Vec<ArrowFunctionSignature> {
    vec![ArrowFunctionSignature::exact(vec![DataType::Utf8; arity], DataType::Utf8)]
}

/// `SOUNDEX`: American Soundex, as Snowflake computes it.
struct Soundex;

impl VArrowScalar for Soundex {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |s, _| Ok(Some(soundex(s, false))))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(1)
    }
}

/// `SOUNDEX_P123(s)`: SOUNDEX that keeps the second letter's digit when it
/// matches the first letter's (`Pfister` is P123, not P236).
struct SoundexP123;

impl VArrowScalar for SoundexP123 {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |s, _| Ok(Some(soundex(s, true))))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(1)
    }
}

/// `SYSTEM$WAIT(amount, unit)`: sleeps, then reports `waited 2 minutes`. The
/// third argument is a volatile nonce, so DuckDB doesn't fold (and run) the
/// call when it only prepares the statement.
struct Wait;

impl VArrowScalar for Wait {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let amounts = input.column(0).as_primitive::<Float64Type>();
        let units = input.column(1).as_string::<i32>();
        let out: StringArray = (0..input.num_rows())
            .map(|i| -> Result<Option<String>> {
                if amounts.is_null(i) || units.is_null(i) {
                    return Ok(None);
                }
                let (amount, unit) = (amounts.value(i), units.value(i).to_lowercase());
                let seconds = match unit.as_str() {
                    "days" => 86_400.0,
                    "hours" => 3_600.0,
                    "minutes" => 60.0,
                    "seconds" => 1.0,
                    "milliseconds" => 1e-3,
                    "microseconds" => 1e-6,
                    "nanoseconds" => 1e-9,
                    _ => return Err(format!("invalid time unit '{unit}' for SYSTEM$WAIT").into()),
                };
                std::thread::sleep(std::time::Duration::from_secs_f64((amount * seconds).max(0.0)));
                let amount = if amount.fract() == 0.0 { format!("{}", amount as i64) } else { amount.to_string() };
                Ok(Some(format!("waited {amount} {unit}")))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Float64, DataType::Utf8, DataType::Float64], DataType::Utf8)]
    }
}

fn soundex(s: &str, p123: bool) -> String {
    let code = |c: char| match c.to_ascii_lowercase() {
        'b' | 'f' | 'p' | 'v' => Some('1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some('2'),
        'd' | 't' => Some('3'),
        'l' => Some('4'),
        'm' | 'n' => Some('5'),
        'r' => Some('6'),
        _ => None,
    };
    let mut letters = s.chars().filter(char::is_ascii_alphabetic);
    let Some(first) = letters.next() else { return String::new() };
    let mut out = first.to_ascii_uppercase().to_string();
    let mut last = if p123 { None } else { code(first) };
    for c in letters {
        let digit = code(c);
        if let Some(d) = digit
            && digit != last
        {
            out.push(d);
            if out.len() == 4 {
                break;
            }
        }
        // H and W don't separate letters with the same code; vowels do.
        if !matches!(c.to_ascii_lowercase(), 'h' | 'w') {
            last = digit;
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

/// `SHA2(s, bits)` for 224, 256, 384 and 512 bits, as lower-case hex.
struct Sha2;

impl VArrowScalar for Sha2 {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        use sha2::Digest;
        let values = input.column(0).as_string::<i32>();
        let bits = input.column(1).as_primitive::<Int64Type>();
        let out: StringArray = values
            .iter()
            .zip(bits.iter())
            .map(|(s, bits)| {
                let (Some(s), Some(bits)) = (s, bits) else { return Ok(None) };
                let digest: Vec<u8> = match bits {
                    224 => sha2::Sha224::digest(s).to_vec(),
                    0 | 256 => sha2::Sha256::digest(s).to_vec(),
                    384 => sha2::Sha384::digest(s).to_vec(),
                    512 => sha2::Sha512::digest(s).to_vec(),
                    other => return Err(format!("Invalid SHA2 digest size: {other}").into()),
                };
                Ok(Some(digest.iter().map(|b| format!("{b:02x}")).collect::<String>()))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Int64], DataType::Utf8)]
    }
}

/// `INITCAP(s[, delimiters])`: upper-cases the first letter after each
/// delimiter and lower-cases the rest. Snowflake's default delimiters are
/// whitespace and most ASCII punctuation.
struct InitCap;

const INITCAP_DELIMITERS: &str = " \t\n\r\x0b\x0c!?@\"^#$&~_,.:;+-*%/|\\[](){}<>";

impl VArrowScalar for InitCap {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |s, delimiters| {
            let delimiters = delimiters.unwrap_or(INITCAP_DELIMITERS);
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if start {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
                start = delimiters.contains(c);
            }
            Ok(Some(out))
        })
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        let mut sigs = strings(1);
        sigs.extend(strings(2));
        sigs
    }
}

/// `PARSE_URL(url)`: the URL's parts as a JSON object, cast to VARIANT by the caller.
struct ParseUrl;

impl VArrowScalar for ParseUrl {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |url, _| Ok(Some(parse_url(url)?.to_string())))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(1)
    }
}

fn parse_url(url: &str) -> Result<serde_json::Value> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("Error parsing URL: {url}"))?;
    let (rest, fragment) = match rest.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (rest, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (rest, None),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (rest, None),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    let parameters = query.map(|q| {
        let mut map = serde_json::Map::new();
        for pair in q.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            map.insert(k.to_owned(), serde_json::Value::String(v.to_owned()));
        }
        serde_json::Value::Object(map)
    });
    Ok(serde_json::json!({
        "fragment": fragment,
        "host": host,
        "parameters": parameters,
        "path": path,
        "port": port,
        "query": query,
        "scheme": scheme,
    }))
}

/// `CONVERT_TIMEZONE(source_tz, target_tz, timestamp_ntz)`: wall-clock time
/// in one zone to wall-clock time in another, using the IANA database.
struct ConvertTimezone;

impl VArrowScalar for ConvertTimezone {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        use chrono::TimeZone;
        let (source, target) = (input.column(0).as_string::<i32>(), input.column(1).as_string::<i32>());
        let values = input.column(2).as_primitive::<TimestampMicrosecondType>();
        let zone = |name: &str| -> Result<chrono_tz::Tz> {
            name.parse().map_err(|_| format!("Unknown timezone: {name}").into())
        };
        let out: TimestampMicrosecondArray = (0..values.len())
            .map(|i| {
                if source.is_null(i) || target.is_null(i) || values.is_null(i) {
                    return Ok(None);
                }
                let local =
                    DateTime::from_timestamp_micros(values.value(i)).ok_or("timestamp out of range")?.naive_utc();
                let instant = zone(source.value(i))?
                    .from_local_datetime(&local)
                    .earliest()
                    .ok_or_else(|| format!("{local} does not exist in {}", source.value(i)))?;
                Ok(Some(instant.with_timezone(&zone(target.value(i))?).naive_local().and_utc().timestamp_micros()))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8, ts()], ts())]
    }
}

/// `sf_flatten_recursive(json, path_prefix, mode)`: every element below the
/// input, depth first, as a JSON array of `{k, i, p, v, t}` (key, index, path,
/// value, parent) for `FLATTEN(..., RECURSIVE => TRUE)`.
struct FlattenRecursive;

impl VArrowScalar for FlattenRecursive {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (json, prefix, mode) = (
            input.column(0).as_string::<i32>(),
            input.column(1).as_string::<i32>(),
            input.column(2).as_string::<i32>(),
        );
        let out: StringArray = (0..json.len())
            .map(|i| {
                if json.is_null(i) {
                    return Ok(None);
                }
                let value: serde_json::Value = serde_json::from_str(json.value(i))?;
                let mut items = Vec::new();
                flatten_into(&value, prefix.value(i), mode.value(i), &mut items);
                Ok(Some(serde_json::Value::Array(items).to_string()))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(3)
    }
}

fn flatten_into(value: &serde_json::Value, path: &str, mode: &str, out: &mut Vec<serde_json::Value>) {
    use serde_json::{Value, json};
    let children: Vec<(Value, Value, String, &Value)> = match value {
        Value::Array(items) if mode != "OBJECT" => {
            items.iter().enumerate().map(|(i, v)| (Value::Null, json!(i), format!("{path}[{i}]"), v)).collect()
        }
        Value::Object(map) if mode != "ARRAY" => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            keys.into_iter()
                .map(|k| {
                    let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    (json!(k), Value::Null, child, &map[k])
                })
                .collect()
        }
        _ => vec![],
    };
    for (key, index, child_path, child) in children {
        out.push(json!({"k": key, "i": index, "p": child_path, "v": child, "t": value}));
        flatten_into(child, &child_path, mode, out);
    }
}

/// `TO_CHAR(number, format)` with Snowflake's numeric format models: `9` and
/// `0` digits, `.`/`D` and `,`/`G` separators, `$`, `S`, `MI` and `FM`.
/// Positive numbers get a leading blank for the sign unless `FM`, `S` or `MI`
/// say otherwise; values too wide for the format become `#`s.
struct FormatNumber;

impl VArrowScalar for FormatNumber {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |value, format| Ok(Some(format_number(value, format.unwrap_or(""))?)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(2)
    }
}

/// A decimal number as sign, integer digits and fraction digits.
fn split_number(text: &str) -> Result<(bool, String, String)> {
    let text = text.trim();
    let normalized;
    let text = if text.contains(['e', 'E']) {
        let value: f64 = text.parse().map_err(|_| format!("Numeric value '{text}' is not recognized"))?;
        normalized = format!("{value:.15}");
        normalized.as_str()
    } else {
        text
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(format!("Numeric value '{text}' is not recognized").into());
    }
    Ok((negative, int.trim_start_matches('0').to_owned(), frac.to_owned()))
}

/// Rounds to `places` fraction digits, half away from zero.
fn round_digits(int: &str, frac: &str, places: usize) -> (String, String) {
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().chain(std::iter::repeat(b'0')).take(places)).collect();
    if frac.as_bytes().get(places).is_some_and(|d| *d >= b'5') {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let split = digits.len() - places;
    let int = String::from_utf8(digits[..split].to_vec()).unwrap();
    (int.trim_start_matches('0').to_owned(), String::from_utf8(digits[split..].to_vec()).unwrap())
}

fn format_number(value: &str, format: &str) -> Result<String> {
    let mut fmt = format.to_uppercase();
    let fill_mode = fmt.starts_with("FM");
    if fill_mode {
        fmt.drain(..2);
    }
    let (leading_s, trailing_s, trailing_mi) = (fmt.starts_with('S'), fmt.ends_with('S'), fmt.ends_with("MI"));
    if leading_s {
        fmt.remove(0);
    } else if trailing_s {
        fmt.pop();
    } else if trailing_mi {
        fmt.truncate(fmt.len() - 2);
    }
    let currency = fmt.contains('$');
    let fmt = fmt.replace('$', "");
    let (int_fmt, frac_fmt) = match fmt.find(['.', 'D']) {
        Some(i) => (&fmt[..i], Some(&fmt[i + 1..])),
        None => (fmt.as_str(), None),
    };
    let places = frac_fmt.map_or(0, |f| f.chars().filter(|c| matches!(c, '9' | '0')).count());
    let (negative, int, frac) = split_number(value)?;
    let (int, frac) = round_digits(&int, &frac, places);
    let negative = negative && (int.chars().chain(frac.chars()).any(|c| c != '0'));

    let slots = int_fmt.chars().filter(|c| matches!(c, '9' | '0')).count();
    if int.len() > slots {
        return Ok("#".repeat(format.len() + 1));
    }
    // Leading zeros show from the leftmost `0` in the format onwards.
    let zero_from = int_fmt.find('0').unwrap_or(int_fmt.len());
    let mut body: Vec<char> = Vec::new();
    let mut digits = int.chars().rev();
    let mut remaining = int.len();
    for (i, c) in int_fmt.char_indices().rev() {
        match c {
            '9' | '0' => match digits.next() {
                Some(d) => {
                    body.push(d);
                    remaining -= 1;
                }
                None if i >= zero_from => body.push('0'),
                None => body.push(' '),
            },
            ',' | 'G' => body.push(if remaining > 0 || i > zero_from { ',' } else { ' ' }),
            _ => {}
        }
    }
    body.reverse();
    // A lone zero integer part with only `9`s still shows one 0 when there is no fraction.
    if int.is_empty()
        && frac_fmt.is_none()
        && zero_from == int_fmt.len()
        && let Some(last) = body.iter_mut().rev().find(|c| **c == ' ')
    {
        *last = '0';
    }
    let mut text: String = body.into_iter().collect();
    if frac_fmt.is_some() {
        text.push('.');
        text.push_str(&frac);
    }
    let prefix =
        format!("{}{}", if leading_s { if negative { "-" } else { "+" } } else { "" }, if currency { "$" } else { "" });
    let implicit_sign = !leading_s && !trailing_s && !trailing_mi;
    let sign = if implicit_sign { if negative { "-" } else { " " } } else { "" };
    let start = text.find(|c: char| c != ' ').unwrap_or(text.len());
    let mut out = format!("{}{sign}{prefix}{}", &text[..start], &text[start..]);
    if trailing_s {
        out.push(if negative { '-' } else { '+' });
    } else if trailing_mi {
        out.push(if negative { '-' } else { ' ' });
    }
    Ok(if fill_mode { out.trim().to_owned() } else { out })
}

/// `TO_NUMBER(text, format)`: strips the format's decoration (`$`, group
/// separators, blanks, sign markers) and returns plain numeric text.
struct NumberText;

impl VArrowScalar for NumberText {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |text, _| {
            let trimmed = text.trim();
            let (negative, body) = if let Some(rest) = trimmed.strip_suffix('-') {
                (true, rest)
            } else if let Some(rest) = trimmed.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
                (true, rest)
            } else {
                (false, trimmed)
            };
            let cleaned: String = body.chars().filter(|c| !matches!(c, '$' | ',' | ' ' | '+')).collect();
            let cleaned = if negative { format!("-{cleaned}") } else { cleaned };
            split_number(&cleaned)?;
            Ok(Some(cleaned))
        })
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(2)
    }
}

/// `PARSE_JSON` input: validates the JSON and replaces JSON nulls with the
/// [`crate::encode::JSON_NULL`] marker so they survive the VARIANT conversion.
/// Empty input is SQL NULL, as in Snowflake.
struct JsonNulls;

impl VArrowScalar for JsonNulls {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_string::<i32>();
        let out: StringArray = values
            .iter()
            .map(|text| {
                let Some(text) = text.filter(|t| !t.trim().is_empty()) else { return Ok(None) };
                let mut value: serde_json::Value =
                    serde_json::from_str(text).map_err(|e| format!("Error parsing JSON: {e}, pos {}", e.column()))?;
                mark_nulls(&mut value);
                Ok(Some(value.to_string()))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(1)
    }
}

fn mark_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Null => *value = serde_json::Value::String(crate::encode::JSON_NULL.to_owned()),
        serde_json::Value::Array(items) => items.iter_mut().for_each(mark_nulls),
        serde_json::Value::Object(map) => map.values_mut().for_each(mark_nulls),
        _ => {}
    }
}

/// Compact text of a semi-structured value, as `TO_VARCHAR` gives it.
struct JsonRender;

impl VArrowScalar for JsonRender {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        map_strings(&input, |text, _| Ok(Some(crate::encode::render_json(text, false))))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(1)
    }
}

/// `TYPEOF` of a VARIANT: Snowflake's type name for a DuckDB `variant_typeof`
/// kind. `text` is the value as text, to recognize the JSON null marker.
struct VariantLabel;

impl VArrowScalar for VariantLabel {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let (kinds, texts) = (input.column(0).as_string::<i32>(), input.column(1).as_string::<i32>());
        let out: StringArray = (0..kinds.len())
            .map(|i| {
                if kinds.is_null(i) {
                    return None;
                }
                let text = (!texts.is_null(i)).then(|| texts.value(i));
                variant_label(kinds.value(i), text)
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        strings(2)
    }
}

fn variant_label(kind: &str, text: Option<&str>) -> Option<&'static str> {
    if text == Some(crate::encode::JSON_NULL) {
        return Some("NULL_VALUE");
    }
    let integer = ["TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT"];
    Some(match kind {
        "VARIANT_NULL" => return None,
        k if k.starts_with("OBJECT") => "OBJECT",
        k if k.starts_with("ARRAY") => "ARRAY",
        "BOOL_TRUE" | "BOOL_FALSE" | "BOOLEAN" => "BOOLEAN",
        k if k.starts_with("INT")
            || k.starts_with("UINT")
            || integer.contains(&k)
            || k.starts_with('U') && integer.contains(&&k[1..]) =>
        {
            "INTEGER"
        }
        k if k.starts_with("DECIMAL") => "DECIMAL",
        "FLOAT" | "DOUBLE" => "DOUBLE",
        "VARCHAR" => "VARCHAR",
        "DATE" => "DATE",
        k if k.starts_with("TIMESTAMP") && (k.ends_with("TIME ZONE") || k.ends_with("_TZ")) => "TIMESTAMP_LTZ",
        // TIMESTAMP_TZ is stored as TIMESTAMP_NS (see the `tz` module).
        k if k.starts_with("TIMESTAMP") && (k.contains("NANO") || k.ends_with("_NS")) => "TIMESTAMP_TZ",
        k if k.starts_with("TIMESTAMP") => "TIMESTAMP_NTZ",
        k if k.starts_with("TIME") => "TIME",
        "BLOB" => "BINARY",
        _ => "VARCHAR",
    })
}

/// `COMPRESS(text, method)` with the ZLIB, GZIP and DEFLATE methods.
struct Compress;

impl VArrowScalar for Compress {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        use std::io::Write;
        let (values, methods) = (input.column(0).as_string::<i32>(), input.column(1).as_string::<i32>());
        let out: duckdb::arrow::array::BinaryArray = (0..values.len())
            .map(|i| {
                if values.is_null(i) || methods.is_null(i) {
                    return Ok(None);
                }
                let data = values.value(i).as_bytes();
                let level = flate2::Compression::default();
                let compressed = match methods.value(i).to_uppercase().split('(').next().unwrap_or("").trim() {
                    "ZLIB" => {
                        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), level);
                        e.write_all(data)?;
                        e.finish()?
                    }
                    "GZIP" => {
                        let mut e = flate2::write::GzEncoder::new(Vec::new(), level);
                        e.write_all(data)?;
                        e.finish()?
                    }
                    "DEFLATE" => {
                        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), level);
                        e.write_all(data)?;
                        e.finish()?
                    }
                    other => return Err(format!("Unsupported compression method: {other}").into()),
                };
                Ok(Some(compressed))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8, DataType::Utf8], DataType::Binary)]
    }
}

/// `DECOMPRESS_BINARY(data, method)`.
struct Decompress;

impl VArrowScalar for Decompress {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        use std::io::Read;
        let (values, methods) = (input.column(0).as_binary::<i32>(), input.column(1).as_string::<i32>());
        let out: duckdb::arrow::array::BinaryArray = (0..values.len())
            .map(|i| {
                if values.is_null(i) || methods.is_null(i) {
                    return Ok(None);
                }
                let data = values.value(i);
                let mut out = Vec::new();
                match methods.value(i).to_uppercase().split('(').next().unwrap_or("").trim() {
                    "ZLIB" => flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)?,
                    "GZIP" => flate2::read::GzDecoder::new(data).read_to_end(&mut out)?,
                    "DEFLATE" => flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?,
                    other => return Err(format!("Unsupported compression method: {other}").into()),
                };
                Ok(Some(out))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Binary, DataType::Utf8], DataType::Binary)]
    }
}

/// `sf_cbrt(x)`: the cube root, exact for perfect cubes.
struct Cbrt;

impl VArrowScalar for Cbrt {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_primitive::<Float64Type>();
        let out: Float64Array = values.iter().map(|v| v.map(f64::cbrt)).collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Float64], DataType::Float64)]
    }
}

/// Whether a timestamp string names its offset (`Z`, `+02:00`, `-0800`).
/// Strings without one are in the session's time zone.
struct HasTzOffset;

impl VArrowScalar for HasTzOffset {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_string::<i32>();
        let out: duckdb::arrow::array::BooleanArray = values.iter().map(|v| v.map(has_tz_offset)).collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Utf8], DataType::Boolean)]
    }
}

fn has_tz_offset(text: &str) -> bool {
    let text = text.trim();
    // Only the part after the date can hold an offset (dates contain '-').
    let Some(time_start) = text.find(['T', 't', ' ']) else { return false };
    let time = &text[time_start + 1..];
    time.ends_with(['Z', 'z']) || time.contains(['+', '-']) || time.to_uppercase().ends_with("UTC")
}

fn ts_ns() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, None)
}

/// `sf_tz_offset_text(tz, colon)`: a TIMESTAMP_TZ's offset as `+hhmm`/`+hh:mm`.
struct TzOffsetText;

impl VArrowScalar for TzOffsetText {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let values = input.column(0).as_primitive::<duckdb::arrow::datatypes::TimestampNanosecondType>();
        let colon = input.column(1).as_boolean();
        let out: StringArray = (0..values.len())
            .map(|i| {
                (!values.is_null(i)).then(|| {
                    crate::tz::offset_text(crate::tz::decode(values.value(i)).1, !colon.is_null(i) && colon.value(i))
                })
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![ts_ns(), DataType::Boolean], DataType::Utf8)]
    }
}

/// `sf_tz_in_zone(instant, zone)`: the instant as a TIMESTAMP_TZ in `zone`
/// (two-argument CONVERT_TIMEZONE).
struct TzInZone;

impl VArrowScalar for TzInZone {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let instants = input.column(0).as_primitive::<TimestampMicrosecondType>();
        let zones = input.column(1).as_string::<i32>();
        let out: duckdb::arrow::array::TimestampNanosecondArray = (0..instants.len())
            .map(|i| {
                if instants.is_null(i) || zones.is_null(i) {
                    return Ok(None);
                }
                let zone: chrono_tz::Tz =
                    zones.value(i).parse().map_err(|_| format!("Unknown timezone: {}", zones.value(i)))?;
                Ok(Some(crate::tz::from_instant(instants.value(i), zone)))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![ts_tz(), DataType::Utf8], ts_ns())]
    }
}

/// `sf_tz_rewrap(local, original)`: a wall clock time back as a TIMESTAMP_TZ
/// with `original`'s offset (results of DATE_TRUNC/DATEADD on TIMESTAMP_TZ).
struct TzRewrap;

impl VArrowScalar for TzRewrap {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let locals = input.column(0).as_primitive::<TimestampMicrosecondType>();
        let originals = input.column(1).as_primitive::<duckdb::arrow::datatypes::TimestampNanosecondType>();
        let out: duckdb::arrow::array::TimestampNanosecondArray = (0..locals.len())
            .map(|i| {
                (!locals.is_null(i) && !originals.is_null(i))
                    .then(|| crate::tz::with_offset(locals.value(i), crate::tz::decode(originals.value(i)).1))
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![ts(), ts_ns()], ts_ns())]
    }
}

/// `sf_tz_instant(x)`: the instant of any timestamp-like value: TIMESTAMP_TZ
/// (decoded), TIMESTAMP_LTZ (as is), TIMESTAMP_NTZ and DATE (wall clock in
/// the session's zone) and text (explicit offset, else the session's zone).
struct TzInstant;

impl VArrowScalar for TzInstant {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let zone = crate::tz::session_zone_now();
        let array = input.column(0);
        let instant_of_local = |local: i64| crate::tz::decode(crate::tz::from_local(local, zone)).0;
        let out: TimestampMicrosecondArray = match array.data_type() {
            DataType::Timestamp(TimeUnit::Nanosecond, None) => array
                .as_primitive::<duckdb::arrow::datatypes::TimestampNanosecondType>()
                .iter()
                .map(|v| v.map(|v| crate::tz::decode(v).0))
                .collect(),
            DataType::Timestamp(_, Some(_)) => array.as_primitive::<TimestampMicrosecondType>().clone(),
            DataType::Timestamp(_, None) => {
                array.as_primitive::<TimestampMicrosecondType>().iter().map(|v| v.map(instant_of_local)).collect()
            }
            DataType::Date32 => array
                .as_primitive::<Date32Type>()
                .iter()
                .map(|d| d.map(|d| instant_of_local(i64::from(d) * 86_400 * MICROS)))
                .collect(),
            DataType::Utf8 => array
                .as_string::<i32>()
                .iter()
                .map(|s| s.map(|s| crate::tz::parse(s, zone).map(|v| crate::tz::decode(v).0)).transpose())
                .collect::<std::result::Result<_, String>>()?,
            // Numbers are seconds since the epoch.
            DataType::Int64 | DataType::Float64 => to_micros(array, true)?,
            other => return Err(format!("cannot convert {other} to a timestamp").into()),
        };
        Ok(Arc::new(out.with_timezone("UTC")))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        [ts_ns(), ts_tz(), ts(), DataType::Date32, DataType::Utf8, DataType::Int64, DataType::Float64]
            .into_iter()
            .map(|input| ArrowFunctionSignature::exact(vec![input], ts_tz()))
            .collect()
    }
}

/// `REGEXP_REPLACE(subject, pattern, replacement, position, occurrence, flags)`
/// for a specific occurrence (DuckDB only replaces the first or all).
/// Occurrence 0 replaces every match; `\N` in the replacement is group N.
struct RegexpReplace;

impl VArrowScalar for RegexpReplace {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let text = |i: usize| input.column(i).as_string::<i32>();
        let (subjects, patterns, replacements, flags) = (text(0), text(1), text(2), text(5));
        let (positions, occurrences) =
            (input.column(3).as_primitive::<Int64Type>(), input.column(4).as_primitive::<Int64Type>());
        let out: StringArray = (0..subjects.len())
            .map(|i| {
                if [
                    subjects.is_null(i),
                    patterns.is_null(i),
                    replacements.is_null(i),
                    positions.is_null(i),
                    occurrences.is_null(i),
                ]
                .contains(&true)
                {
                    return Ok(None);
                }
                let options: String = flags.is_null(i).then(String::new).unwrap_or_else(|| flags.value(i).to_owned());
                let mut prefix = String::from("(?");
                for (flag, inline) in [('i', 'i'), ('m', 'm'), ('s', 's')] {
                    if options.contains(flag) && !options[options.rfind(flag).unwrap()..].contains('c') {
                        prefix.push(inline);
                    }
                }
                let pattern = if prefix.len() > 2 {
                    format!("{prefix}){}", patterns.value(i))
                } else {
                    patterns.value(i).to_owned()
                };
                let re = regex::Regex::new(&pattern).map_err(|e| format!("Invalid regular expression: {e}"))?;
                // Snowflake's \1 backreferences become regex's ${1}.
                let replacement =
                    regex::Regex::new(r"\\(\d)").unwrap().replace_all(replacements.value(i), "$${$1}").into_owned();
                let subject = subjects.value(i);
                let start = subject
                    .char_indices()
                    .nth((positions.value(i).max(1) - 1) as usize)
                    .map_or(subject.len(), |(b, _)| b);
                let (head, tail) = subject.split_at(start);
                let occurrence = occurrences.value(i);
                let replaced = if occurrence <= 0 {
                    re.replace_all(tail, replacement.as_str()).into_owned()
                } else {
                    match re.captures_iter(tail).nth((occurrence - 1) as usize) {
                        Some(caps) => {
                            let whole = caps.get(0).unwrap();
                            let mut expanded = String::new();
                            caps.expand(&replacement, &mut expanded);
                            format!("{}{expanded}{}", &tail[..whole.start()], &tail[whole.end()..])
                        }
                        None => tail.to_owned(),
                    }
                };
                Ok(Some(format!("{head}{replaced}")))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(
            vec![DataType::Utf8, DataType::Utf8, DataType::Utf8, DataType::Int64, DataType::Int64, DataType::Utf8],
            DataType::Utf8,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        let micros = |s: &str| parse_timestamp(s, false).unwrap();
        assert_eq!(micros("2024-01-02 03:04:05"), 1_704_164_645 * MICROS);
        assert_eq!(micros("2024-01-02T03:04:05.5"), 1_704_164_645 * MICROS + 500_000);
        assert_eq!(micros("2024-01-02"), 1_704_153_600 * MICROS);
        assert_eq!(micros("1704164645"), 1_704_164_645 * MICROS);
        assert_eq!(micros("1704164645000"), 1_704_164_645 * MICROS);
        // NTZ keeps wall-clock time; LTZ converts to UTC.
        assert_eq!(micros("2024-01-02 03:04:05 +02:00"), 1_704_164_645 * MICROS);
        assert_eq!(parse_timestamp("2024-01-02T03:04:05+02:00", true).unwrap(), (1_704_164_645 - 7200) * MICROS);
        assert_eq!(parse_timestamp("2024-01-02T03:04:05Z", true).unwrap(), 1_704_164_645 * MICROS);
        assert!(parse_timestamp("nope", false).is_err());
    }

    #[test]
    fn number_formats() {
        let f = |v: &str, fmt: &str| super::format_number(v, fmt).unwrap();
        assert_eq!(f("1234.5", "9,999.99"), " 1,234.50");
        assert_eq!(f("0.5", "0.00"), " 0.50");
        assert_eq!(f("42", "000"), " 042");
        assert_eq!(f("-42", "9999"), "  -42");
        assert_eq!(f("12.3456", "99.99"), " 12.35");
        assert_eq!(f("12345", "999"), "####");
        assert_eq!(f("42", "FM9999"), "42");
        assert_eq!(f("-5", "S99"), " -5");
        assert_eq!(f("5", "99MI"), " 5 ");
        assert_eq!(f("42", "$999"), "  $42");
        assert_eq!(f("0", "99"), "  0");
    }

    #[test]
    fn soundex_codes() {
        assert_eq!(super::soundex("Robert", false), "R163");
        assert_eq!(super::soundex("Rupert", false), "R163");
        assert_eq!(super::soundex("Ashcraft", false), "A261");
        assert_eq!(super::soundex("Tymczak", false), "T522");
        assert_eq!(super::soundex("Pfister", false), "P236");
    }

    #[test]
    fn parses_dates() {
        assert_eq!(parse_date("1970-01-02").unwrap(), 1);
        assert_eq!(parse_date("02-Jan-1970").unwrap(), 1);
        assert_eq!(parse_date("1970-01-02 10:00:00").unwrap(), 1);
        assert!(parse_date("nope").is_err());
    }
}
