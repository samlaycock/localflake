//! TIMESTAMP_TZ: instants that keep their UTC offset.
//!
//! DuckDB has no such type, so a TIMESTAMP_TZ is stored as `TIMESTAMP_NS`
//! whose sub-microsecond digits encode the offset in 15-minute steps:
//! `instant_us * 1000 + (offset / 15 min + 56)`. Values still order by
//! instant, and microsecond precision is all localflake keeps anyway.
//!
//! Conversions are registered as DuckDB casts, so `CAST`, implicit casts and
//! INSERTs into TIMESTAMP_TZ columns all go through them:
//! VARCHAR/TIMESTAMP/TIMESTAMPTZ/DATE → TIMESTAMP_TZ, and TIMESTAMP_TZ →
//! TIMESTAMP (local wall time), TIMESTAMPTZ (instant), DATE, TIME and VARCHAR.
//!
//! Strings without an offset, NTZ values and LTZ values take the session's
//! time zone. Casts can't see which session they run for, so the engine
//! publishes the zone of the statement it is executing ([`enter`]).

use std::ptr;
use std::sync::{Condvar, Mutex};

use chrono::{DateTime, Offset, TimeZone};
use chrono_tz::Tz;
use duckdb::ffi;

const MICROS: i64 = 1_000_000;
/// The offset code for UTC; codes 0..=112 cover -14:00..+14:00.
const UTC_CODE: i64 = 56;

/// The zone casts use, and how many statements are running with it. Casts
/// can't tell which session they run for, so statements in different zones
/// take turns while statements in the same zone run concurrently.
static SESSION_ZONE: Mutex<(Tz, usize)> = Mutex::new((chrono_tz::UTC, 0));
static ZONE_FREE: Condvar = Condvar::new();

/// Holds the session zone for one statement; see [`enter`].
pub struct ZoneGuard;

impl Drop for ZoneGuard {
    fn drop(&mut self) {
        let mut slot = SESSION_ZONE.lock().unwrap();
        slot.1 -= 1;
        if slot.1 == 0 {
            ZONE_FREE.notify_all();
        }
    }
}

/// Makes `name` the zone for values without an explicit offset until the
/// guard drops, waiting while statements in another zone are running.
pub fn enter(name: &str) -> ZoneGuard {
    let zone = name.parse::<Tz>().unwrap_or(chrono_tz::UTC);
    let mut slot = SESSION_ZONE.lock().unwrap();
    while slot.1 > 0 && slot.0 != zone {
        slot = ZONE_FREE.wait(slot).unwrap();
    }
    slot.0 = zone;
    slot.1 += 1;
    ZoneGuard
}

fn session_zone() -> Tz {
    SESSION_ZONE.lock().unwrap().0
}

pub fn session_zone_now() -> Tz {
    session_zone()
}

pub fn encode(instant_us: i64, offset_seconds: i32) -> i64 {
    let code = (i64::from(offset_seconds) / 900 + UTC_CODE).clamp(0, 2 * UTC_CODE);
    instant_us * 1000 + code
}

/// `(instant in microseconds, offset in seconds)`.
pub fn decode(value: i64) -> (i64, i32) {
    let instant = value.div_euclid(1000);
    let code = value.rem_euclid(1000).min(2 * UTC_CODE);
    (instant, ((code - UTC_CODE) * 900) as i32)
}

/// Wall clock microseconds at the value's own offset.
pub fn local_micros(value: i64) -> i64 {
    let (instant, offset) = decode(value);
    instant + i64::from(offset) * MICROS
}

fn offset_at_instant(zone: Tz, instant_us: i64) -> i32 {
    let utc = DateTime::from_timestamp_micros(instant_us).unwrap_or_default().naive_utc();
    zone.offset_from_utc_datetime(&utc).fix().local_minus_utc()
}

/// A wall clock time in `zone` as a TIMESTAMP_TZ.
pub fn from_local(local_us: i64, zone: Tz) -> i64 {
    let naive = DateTime::from_timestamp_micros(local_us).unwrap_or_default().naive_utc();
    let offset = zone.offset_from_local_datetime(&naive).earliest().map_or(0, |o| o.fix().local_minus_utc());
    encode(local_us - i64::from(offset) * MICROS, offset)
}

/// An instant as a TIMESTAMP_TZ with `zone`'s offset at that instant.
pub fn from_instant(instant_us: i64, zone: Tz) -> i64 {
    encode(instant_us, offset_at_instant(zone, instant_us))
}

/// `+hhmm` (or `+hh:mm`) for an offset in seconds.
pub fn offset_text(offset_seconds: i32, colon: bool) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let minutes = offset_seconds.unsigned_abs() / 60;
    let sep = if colon { ":" } else { "" };
    format!("{sign}{:02}{sep}{:02}", minutes / 60, minutes % 60)
}

/// Snowflake's default TIMESTAMP_TZ text: `YYYY-MM-DD HH24:MI:SS.FF3 TZHTZM`.
pub fn format(value: i64) -> String {
    let (_, offset) = decode(value);
    let local = DateTime::from_timestamp_micros(local_micros(value)).unwrap_or_default().naive_utc();
    format!("{} {}", local.format("%Y-%m-%d %H:%M:%S%.3f"), offset_text(offset, false))
}

/// Parses timestamp text, keeping an explicit offset; text without one is a
/// wall clock time in `zone`, and integer text is seconds since the epoch.
pub fn parse(text: &str, zone: Tz) -> Result<i64, String> {
    let s = text.trim();
    if s.parse::<i64>().is_ok() {
        let micros = crate::functions::parse_timestamp(s, true).map_err(|e| e.to_string())?;
        return Ok(from_instant(micros, zone));
    }
    if let Some((local, offset)) = crate::functions::parse_with_offset(s) {
        return Ok(encode(local - i64::from(offset) * MICROS, offset));
    }
    let local = crate::functions::parse_timestamp(s, false).map_err(|e| e.to_string())?;
    Ok(from_local(local, zone))
}

// ---- casts -----------------------------------------------------------------

type CastFn =
    unsafe extern "C" fn(ffi::duckdb_function_info, ffi::idx_t, ffi::duckdb_vector, ffi::duckdb_vector) -> bool;

/// Registers the TIMESTAMP_TZ casts on `db`.
///
/// # Safety
/// `db` must be an open database.
pub unsafe fn register(db: ffi::duckdb_database) -> Result<(), String> {
    use ffi::*;
    let casts: &[(duckdb_type, duckdb_type, CastFn, i64)] = &[
        (DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, varchar_to_tz, -1),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, ntz_to_tz, -1),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, ltz_to_tz, -1),
        (DUCKDB_TYPE_DUCKDB_TYPE_DATE, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, date_to_tz, -1),
        // Implicit, so functions taking TIMESTAMP see TIMESTAMP_TZ values in their own local time.
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP, tz_to_ntz, 1),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ, tz_to_ltz, 2),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, DUCKDB_TYPE_DUCKDB_TYPE_DATE, tz_to_date, -1),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, DUCKDB_TYPE_DUCKDB_TYPE_TIME, tz_to_time, -1),
        (DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS, DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR, tz_to_varchar, -1),
    ];
    unsafe {
        let mut con: duckdb_connection = ptr::null_mut();
        if duckdb_connect(db, &mut con) != duckdb_state_DuckDBSuccess {
            return Err("cannot connect to register TIMESTAMP_TZ casts".into());
        }
        let mut result = Ok(());
        for &(source, target, function, cost) in casts {
            let mut cast = duckdb_create_cast_function();
            let mut s = duckdb_create_logical_type(source);
            let mut t = duckdb_create_logical_type(target);
            duckdb_cast_function_set_source_type(cast, s);
            duckdb_cast_function_set_target_type(cast, t);
            duckdb_cast_function_set_implicit_cast_cost(cast, cost);
            duckdb_cast_function_set_function(cast, Some(function));
            if duckdb_register_cast_function(con, cast) != duckdb_state_DuckDBSuccess {
                result = Err("cannot register a TIMESTAMP_TZ cast".to_owned());
            }
            duckdb_destroy_logical_type(&mut s);
            duckdb_destroy_logical_type(&mut t);
            duckdb_destroy_cast_function(&mut cast);
        }
        duckdb_disconnect(&mut con);
        result
    }
}

/// Runs `convert` over a fixed-width input vector, writing a fixed-width output.
unsafe fn map_fixed<I: Copy, O>(
    info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
    convert: impl Fn(I) -> Result<O, String>,
) -> bool {
    unsafe {
        let data = ffi::duckdb_vector_get_data(input) as *const I;
        let out = ffi::duckdb_vector_get_data(output) as *mut O;
        let validity = ffi::duckdb_vector_get_validity(input);
        ffi::duckdb_vector_ensure_validity_writable(output);
        let out_validity = ffi::duckdb_vector_get_validity(output);
        for row in 0..count {
            if !validity.is_null() && !ffi::duckdb_validity_row_is_valid(validity, row) {
                ffi::duckdb_validity_set_row_invalid(out_validity, row);
                continue;
            }
            match convert(*data.add(row as usize)) {
                Ok(value) => out.add(row as usize).write(value),
                Err(message) => return fail(info, &message, row, output),
            }
        }
        true
    }
}

/// Reports a conversion error: NULL under TRY_CAST, otherwise a failed cast.
unsafe fn fail(info: ffi::duckdb_function_info, message: &str, row: ffi::idx_t, output: ffi::duckdb_vector) -> bool {
    let message = std::ffi::CString::new(message).unwrap_or_default();
    unsafe {
        if ffi::duckdb_cast_function_get_cast_mode(info) == ffi::duckdb_cast_mode_DUCKDB_CAST_TRY {
            ffi::duckdb_cast_function_set_row_error(info, message.as_ptr(), row, output);
            true
        } else {
            ffi::duckdb_cast_function_set_error(info, message.as_ptr());
            false
        }
    }
}

unsafe extern "C" fn varchar_to_tz(
    info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    let zone = session_zone();
    unsafe {
        let data = ffi::duckdb_vector_get_data(input) as *mut ffi::duckdb_string_t;
        let out = ffi::duckdb_vector_get_data(output) as *mut i64;
        let validity = ffi::duckdb_vector_get_validity(input);
        ffi::duckdb_vector_ensure_validity_writable(output);
        let out_validity = ffi::duckdb_vector_get_validity(output);
        for row in 0..count {
            if !validity.is_null() && !ffi::duckdb_validity_row_is_valid(validity, row) {
                ffi::duckdb_validity_set_row_invalid(out_validity, row);
                continue;
            }
            let value = data.add(row as usize);
            let len = ffi::duckdb_string_t_length(*value) as usize;
            let bytes = std::slice::from_raw_parts(ffi::duckdb_string_t_data(value) as *const u8, len);
            let text = String::from_utf8_lossy(bytes);
            match parse(&text, zone) {
                Ok(v) => out.add(row as usize).write(v),
                Err(_) => return fail(info, &format!("Timestamp '{text}' is not recognized"), row, output),
            }
        }
        true
    }
}

unsafe extern "C" fn ntz_to_tz(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    let zone = session_zone();
    unsafe { map_fixed(info, n, i, o, |us: i64| Ok(from_local(us, zone))) }
}

unsafe extern "C" fn ltz_to_tz(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    let zone = session_zone();
    unsafe { map_fixed(info, n, i, o, |us: i64| Ok(from_instant(us, zone))) }
}

unsafe extern "C" fn date_to_tz(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    let zone = session_zone();
    unsafe { map_fixed(info, n, i, o, |days: i32| Ok(from_local(i64::from(days) * 86_400 * MICROS, zone))) }
}

unsafe extern "C" fn tz_to_ntz(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    unsafe { map_fixed(info, n, i, o, |v: i64| Ok(local_micros(v))) }
}

unsafe extern "C" fn tz_to_ltz(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    unsafe { map_fixed(info, n, i, o, |v: i64| Ok(decode(v).0)) }
}

unsafe extern "C" fn tz_to_date(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    unsafe { map_fixed(info, n, i, o, |v: i64| Ok(local_micros(v).div_euclid(86_400 * MICROS) as i32)) }
}

unsafe extern "C" fn tz_to_time(
    info: ffi::duckdb_function_info,
    n: ffi::idx_t,
    i: ffi::duckdb_vector,
    o: ffi::duckdb_vector,
) -> bool {
    unsafe { map_fixed(info, n, i, o, |v: i64| Ok(local_micros(v).rem_euclid(86_400 * MICROS))) }
}

unsafe extern "C" fn tz_to_varchar(
    info: ffi::duckdb_function_info,
    count: ffi::idx_t,
    input: ffi::duckdb_vector,
    output: ffi::duckdb_vector,
) -> bool {
    let _ = info;
    unsafe {
        let data = ffi::duckdb_vector_get_data(input) as *const i64;
        let validity = ffi::duckdb_vector_get_validity(input);
        ffi::duckdb_vector_ensure_validity_writable(output);
        let out_validity = ffi::duckdb_vector_get_validity(output);
        for row in 0..count {
            if !validity.is_null() && !ffi::duckdb_validity_row_is_valid(validity, row) {
                ffi::duckdb_validity_set_row_invalid(out_validity, row);
                continue;
            }
            let text = format(*data.add(row as usize));
            ffi::duckdb_vector_assign_string_element_len(
                output,
                row,
                text.as_ptr() as *const _,
                text.len() as ffi::idx_t,
            );
        }
        true
    }
}

/// Wall clock time of `local` at `offset_seconds`, for re-wrapping results
/// like `DATE_TRUNC` or `DATEADD` of a TIMESTAMP_TZ.
pub fn with_offset(local_us: i64, offset_seconds: i32) -> i64 {
    encode(local_us - i64::from(offset_seconds) * MICROS, offset_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_offsets() {
        for offset in [-14 * 3600, -5 * 3600, 0, 5 * 3600 + 45 * 60, 14 * 3600] {
            let v = encode(1_700_000_000_123_456, offset);
            assert_eq!(decode(v), (1_700_000_000_123_456, offset));
        }
        assert!(encode(1, 3600) > encode(0, -3600), "orders by instant");
    }

    #[test]
    fn parses_and_formats() {
        let v = parse("2024-03-15 10:11:12 +02:00", chrono_tz::UTC).unwrap();
        assert_eq!(format(v), "2024-03-15 10:11:12.000 +0200");
        let ny: Tz = "America/New_York".parse().unwrap();
        assert_eq!(format(parse("2024-07-01 09:30", ny).unwrap()), "2024-07-01 09:30:00.000 -0400");
    }
}
