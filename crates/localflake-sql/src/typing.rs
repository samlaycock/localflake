//! Type-directed rewrites.
//!
//! A few Snowflake results depend on operand types: `a / b` and `AVG(x)` have
//! a scale derived from the operands' scales, `MEDIAN(x)` adds 3 to the scale.
//! The translator runs twice for statements with such sites. The first pass
//! emits `sf_probe(site_id, operands...)` at each site; the engine prepares
//! that SQL, which records the operand types (see the engine's `probe`
//! module), and the second pass emits SQL for the actual types.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use sqlparser::ast::{BinaryOperator, DataType, ExactNumberInfo, Expr, Function, Value};

use crate::Context;
use crate::ast::*;
use crate::datetime::convert_format;

/// Operand types per site, shared by both translation passes.
#[derive(Debug, Clone, Default)]
pub struct SiteTypes {
    mode: RefCell<Mode>,
    next: Cell<u32>,
    /// `SEQ*` calls in the current pass; each gets its own session sequence.
    sequences: Cell<u32>,
    /// Sites probed in a refine pass.
    unknown: Cell<u32>,
}

#[derive(Debug, Clone, Default)]
enum Mode {
    /// Types unknown and not probed: sites fall back to untyped SQL.
    #[default]
    Off,
    Probe,
    /// Known types, probing the sites that don't have one yet.
    Refine(HashMap<u32, Vec<String>>),
    Known(HashMap<u32, Vec<String>>),
}

impl SiteTypes {
    /// Starts a probe pass.
    pub fn probe(&self) {
        self.next.set(0);
        self.sequences.set(0);
        *self.mode.borrow_mut() = Mode::Probe;
    }

    /// Starts a pass that uses `types` and probes the sites still missing.
    pub fn refine(&self, types: HashMap<u32, Vec<String>>) {
        self.next.set(0);
        self.sequences.set(0);
        self.unknown.set(0);
        *self.mode.borrow_mut() = Mode::Refine(types);
    }

    /// Sites probed in the current pass for lack of a type.
    pub fn unknown(&self) -> u32 {
        self.unknown.get()
    }

    /// Starts a pass with the types a probe recorded.
    pub fn known(&self, types: HashMap<u32, Vec<String>>) {
        self.next.set(0);
        self.sequences.set(0);
        *self.mode.borrow_mut() = Mode::Known(types);
    }

    pub fn off(&self) {
        self.next.set(0);
        self.sequences.set(0);
        *self.mode.borrow_mut() = Mode::Off;
    }

    /// Allocates the session sequence for a `SEQ*` call (see [`SEQUENCES`]).
    pub fn next_sequence(&self) -> u32 {
        let n = self.sequences.get();
        self.sequences.set(n + 1);
        n
    }

    pub fn sequences(&self) -> u32 {
        self.sequences.get()
    }

    /// The number of sites seen in the current pass.
    pub fn sites(&self) -> u32 {
        self.next.get()
    }

    fn site(&self, operands: &[Expr]) -> Site {
        match self.raw_site(operands, "sf_probe") {
            RawSite::Probe(probe) => Site::Probe(probe),
            RawSite::Typed(types) => Site::Typed(types.iter().map(|t| NumType::parse(t)).collect()),
            RawSite::Unknown => Site::Unknown,
        }
    }

    fn raw_site(&self, operands: &[Expr], probe: &str) -> RawSite {
        let id = self.next.get();
        self.next.set(id + 1);
        match &*self.mode.borrow() {
            Mode::Off => RawSite::Unknown,
            Mode::Probe => {
                let mut args = vec![number(id)];
                args.extend(operands.iter().cloned());
                RawSite::Probe(Box::new(func(probe, args)))
            }
            Mode::Known(known) => known.get(&id).map_or(RawSite::Unknown, |t| RawSite::Typed(t.clone())),
            Mode::Refine(known) => match known.get(&id) {
                Some(t) => RawSite::Typed(t.clone()),
                None => {
                    self.unknown.set(self.unknown.get() + 1);
                    let mut args = vec![number(id)];
                    args.extend(operands.iter().cloned());
                    RawSite::Probe(Box::new(func(probe, args)))
                }
            },
        }
    }
}

enum Site {
    Probe(Box<Expr>),
    Typed(Vec<NumType>),
    Unknown,
}

enum RawSite {
    Probe(Box<Expr>),
    Typed(Vec<String>),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum NumType {
    /// Integers and decimals, with their scale.
    Fixed(u8),
    /// Floats, and strings (which Snowflake converts in arithmetic).
    Float,
    Other,
}

impl NumType {
    fn parse(name: &str) -> Self {
        if let Some(rest) = name.strip_prefix("DECIMAL(") {
            let scale = rest.trim_end_matches(')').split(',').nth(1).and_then(|s| s.trim().parse().ok());
            return scale.map_or(Self::Other, Self::Fixed);
        }
        match name {
            "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
            | "UBIGINT" | "UHUGEINT" | "NULL" => Self::Fixed(0),
            "FLOAT" | "DOUBLE" | "VARCHAR" => Self::Float,
            _ => Self::Other,
        }
    }
}

/// `a / b`: decimal division with Snowflake's result scale
/// (`max(s1, min(s1 + 6, 12))`, rounded half away from zero), float division
/// for floats, and a division-by-zero error either way.
pub fn divide(types: &SiteTypes, left: Expr, right: Expr) -> Expr {
    match types.site(&[left.clone(), right.clone()]) {
        Site::Probe(probe) => *probe,
        Site::Typed(t) => match (t[0], t[1]) {
            (NumType::Fixed(s1), NumType::Fixed(s2)) => {
                fixed_divide(left.clone(), s1, right.clone(), s2).unwrap_or_else(|| float_divide(left, right))
            }
            (NumType::Other, _) | (_, NumType::Other) => func("sf_div", vec![left, right]),
            _ => float_divide(left, right),
        },
        Site::Unknown => func("sf_div", vec![left, right]),
    }
}

fn float_divide(left: Expr, right: Expr) -> Expr {
    let double = || DataType::Double(ExactNumberInfo::None);
    func("sf_div", vec![cast(left, double()), cast(right, double())])
}

/// Exact decimal division: both operands as unscaled integers, the quotient
/// computed in HUGEINT arithmetic, then scaled back with a typed unit literal.
fn fixed_divide(left: Expr, s1: u8, right: Expr, s2: u8) -> Option<Expr> {
    let scale = s1.max((s1 + 6).min(12));
    let shift = u32::from(scale) + u32::from(s2) - u32::from(s1);
    if shift > 30 {
        return None;
    }
    // Twice the shift factor: the extra digit drives rounding half away from zero.
    let factor = 2 * 10_i128.pow(shift);
    let quotient = func(
        "sf_div_scaled",
        vec![unscaled(left, s1), unscaled(right, s2), Expr::Value(Value::Number(factor.to_string(), false).into())],
    );
    Some(binop(cast(quotient, decimal(0)), BinaryOperator::Multiply, unit(scale)))
}

fn unscaled(value: Expr, scale: u8) -> Expr {
    let value =
        if scale == 0 { value } else { binop(value, BinaryOperator::Multiply, number(10_i128.pow(scale.into()))) };
    cast(value, DataType::HugeInt)
}

/// `10^-scale` typed as `DECIMAL(38, scale)`.
fn unit(scale: u8) -> Expr {
    let text = if scale == 0 { "1".to_owned() } else { format!("0.{}1", "0".repeat(usize::from(scale) - 1)) };
    cast(string(text), decimal(scale))
}

fn decimal(scale: u8) -> DataType {
    DataType::Decimal(ExactNumberInfo::PrecisionAndScale(38, i64::from(scale)))
}

/// `AVG(x)`: for integers and decimals, `SUM(x) / COUNT(x)` with division's
/// scale rule, so `AVG` of integers is `NUMBER(38,6)` as in Snowflake.
pub fn avg(types: &SiteTypes, f: &mut Function, arg: Expr) -> Option<Expr> {
    match types.site(std::slice::from_ref(&arg)) {
        Site::Probe(probe) => {
            replace_first_arg(f, *probe);
            None
        }
        Site::Typed(t) => match t[0] {
            NumType::Fixed(scale) => {
                let sum = renamed(f, "sum");
                let count = renamed(f, "count");
                fixed_divide(sum, scale, count, 0)
            }
            _ => None,
        },
        Site::Unknown => None,
    }
}

/// `MEDIAN(x)`: integers and decimals gain 3 digits of scale.
pub fn median(types: &SiteTypes, f: &mut Function, arg: Expr) -> Option<Expr> {
    match types.site(std::slice::from_ref(&arg)) {
        Site::Probe(probe) => {
            replace_first_arg(f, *probe);
            None
        }
        Site::Typed(t) => match t[0] {
            NumType::Fixed(scale) => Some(cast(renamed(f, "median"), decimal(scale + 3))),
            _ => None,
        },
        Site::Unknown => None,
    }
}

fn renamed(f: &Function, name: &str) -> Expr {
    let mut f = f.clone();
    f.name = object_name(&[name]);
    Expr::Function(f)
}

fn replace_first_arg(f: &mut Function, value: Expr) {
    if let sqlparser::ast::FunctionArguments::List(list) = &mut f.args
        && let Some(sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(first))) =
            list.args.first_mut()
    {
        *first = value;
    }
}

/// Conversion to text as Snowflake does it implicitly and in `TO_VARCHAR(x)`:
/// timestamps, dates and times use the session's output formats, floats drop
/// a trailing `.0`, everything else goes through `sf_to_varchar`.
pub fn text(ctx: &Context, value: Expr) -> Expr {
    if string_literal(&value).is_some() {
        return value;
    }
    let types = &ctx.types;
    let type_name = match types.raw_site(std::slice::from_ref(&value), "sf_probe_text") {
        RawSite::Probe(probe) => return *probe,
        RawSite::Typed(t) => t.into_iter().next().unwrap_or_default(),
        RawSite::Unknown => String::new(),
    };
    let ntz = || {
        let own = parameter(ctx, "TIMESTAMP_NTZ_OUTPUT_FORMAT");
        if own.is_empty() {
            parameter(ctx, "TIMESTAMP_OUTPUT_FORMAT").replace(" TZHTZM", "").replace(" TZH:TZM", "")
        } else {
            own
        }
    };
    let ltz_format = || {
        let own = parameter(ctx, "TIMESTAMP_LTZ_OUTPUT_FORMAT");
        if own.is_empty() { parameter(ctx, "TIMESTAMP_OUTPUT_FORMAT") } else { own }
    };
    let formatted = |value: Expr, format: String| match convert_format(&format) {
        Some(strftime) => func("strftime", vec![value, string(strftime)]),
        None => func("sf_to_varchar", vec![value]),
    };
    let timestamp = |v: Expr| cast(v, DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::None));
    match type_name.as_str() {
        "TIMESTAMP" => formatted(value, ntz()),
        "TIMESTAMP_NS" => {
            // TIMESTAMP_TZ: local time at the value's own offset, then that offset.
            let own = parameter(ctx, "TIMESTAMP_TZ_OUTPUT_FORMAT");
            let format = if own.is_empty() { parameter(ctx, "TIMESTAMP_OUTPUT_FORMAT") } else { own };
            let (token, colon) = if format.contains("TZH:TZM") { ("TZH:TZM", true) } else { ("TZHTZM", false) };
            match format.split_once(token) {
                Some((before, after)) => {
                    let offset = func("sf_tz_offset_text", vec![value.clone(), boolean(colon)]);
                    let mut text = binop(
                        formatted(timestamp(value.clone()), before.to_owned()),
                        BinaryOperator::StringConcat,
                        offset,
                    );
                    if !after.is_empty() {
                        text = binop(text, BinaryOperator::StringConcat, formatted(timestamp(value), after.to_owned()));
                    }
                    text
                }
                None => formatted(timestamp(value), format),
            }
        }
        "TIMESTAMPTZ" => {
            // Local wall time in the session's time zone, then its offset.
            let format = ltz_format();
            let (token, colon) = if format.contains("TZH:TZM") { ("TZH:TZM", true) } else { ("TZHTZM", false) };
            match format.split_once(token) {
                Some((before, after)) => {
                    let offset = func("sf_tz_offset", vec![value.clone(), boolean(colon)]);
                    let mut text = binop(
                        formatted(timestamp(value.clone()), before.to_owned()),
                        BinaryOperator::StringConcat,
                        offset,
                    );
                    if !after.is_empty() {
                        text = binop(text, BinaryOperator::StringConcat, formatted(timestamp(value), after.to_owned()));
                    }
                    text
                }
                None => formatted(timestamp(value), format),
            }
        }
        "DATE" => formatted(value, parameter(ctx, "DATE_OUTPUT_FORMAT")),
        "TIME" => {
            let epoch = cast(string("1970-01-01"), DataType::Date);
            formatted(binop(epoch, BinaryOperator::Plus, value), parameter(ctx, "TIME_OUTPUT_FORMAT"))
        }
        "DOUBLE" | "FLOAT" => func("sf_float_text", vec![value]),
        "VARCHAR" => value,
        "VARIANT" => func("sf_variant_text_value", vec![value]),
        _ => func("sf_to_varchar", vec![value]),
    }
}

/// Conversion to TIMESTAMP_TZ. Temporal values and text go through the
/// engine's registered casts (which keep explicit offsets and otherwise use
/// the session time zone); numbers are epoch instants.
pub fn to_timestamp_tz(ctx: &Context, value: Expr) -> Expr {
    let target = || custom_type("TIMESTAMP_NS");
    let type_name = match ctx.types.raw_site(std::slice::from_ref(&value), "sf_probe_text") {
        RawSite::Probe(probe) => return cast(*probe, target()),
        RawSite::Typed(t) => t.into_iter().next().unwrap_or_default(),
        RawSite::Unknown => String::new(),
    };
    match NumType::parse(&type_name) {
        NumType::Fixed(_) => cast(func("sf_ltz", vec![value]), target()),
        _ if matches!(type_name.as_str(), "FLOAT" | "DOUBLE") => cast(func("sf_ltz", vec![value]), target()),
        _ if type_name == "VARIANT" => cast(cast(value, DataType::Varchar(None)), target()),
        _ => cast(value, target()),
    }
}

/// Applies a date/time operation that Snowflake performs on TIMESTAMP_TZ in
/// the value's local time and returns as TIMESTAMP_TZ with the same offset
/// (`DATE_TRUNC`, `DATEADD`). Other types pass straight to `op`.
pub fn tz_preserving(
    ctx: &Context,
    value: Expr,
    op: impl FnOnce(Expr) -> Result<Expr, crate::SqlError>,
) -> Result<Expr, crate::SqlError> {
    let local = |v: Expr| cast(v, DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::None));
    match ctx.types.raw_site(std::slice::from_ref(&value), "sf_probe_text") {
        RawSite::Probe(probe) => op(local(*probe)),
        RawSite::Typed(t) if t.first().is_some_and(|t| t == "TIMESTAMP_NS") => {
            Ok(func("sf_tz_rewrap", vec![op(local(value.clone()))?, value]))
        }
        _ => op(value),
    }
}

/// A TIMESTAMP_TZ as its UTC wall clock time (Snowflake compares TIMESTAMP_TZ
/// values in UTC, e.g. in DATEDIFF); other values unchanged.
pub fn utc_if_timestamp_tz(ctx: &Context, value: Expr) -> Expr {
    match ctx.types.raw_site(std::slice::from_ref(&value), "sf_probe_text") {
        RawSite::Probe(probe) => cast(*probe, DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::None)),
        RawSite::Typed(t) if t.first().is_some_and(|t| t == "TIMESTAMP_NS") => {
            func("make_timestamp", vec![func("epoch_us", vec![func("sf_tz_instant", vec![value])])])
        }
        _ => value,
    }
}

/// `SYSTEM$TYPEOF(x)`: Snowflake's type with its storage class, e.g.
/// `NUMBER(1,0)[SB1]`. Literals are described exactly; other expressions by
/// their type, with the widest storage class Snowflake would use.
pub fn system_typeof(ctx: &Context, value: Expr) -> Expr {
    if let Expr::Value(v) = &value {
        match &v.value {
            Value::Number(text, _) if !text.contains(['e', 'E']) => {
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                let scale = text.split_once('.').map_or(0, |(_, f)| f.len());
                let precision = digits.trim_start_matches('0').len().max(1).max(scale);
                let unscaled: i128 = digits.parse().unwrap_or(i128::MAX);
                return string(format!("NUMBER({precision},{scale})[{}]", storage_class(unscaled)));
            }
            Value::SingleQuotedString(s)
            | Value::DollarQuotedString(sqlparser::ast::DollarQuotedString { value: s, .. }) => {
                return string(format!("VARCHAR({})[LOB]", s.chars().count()));
            }
            Value::Boolean(_) => return string("BOOLEAN[SB1]"),
            Value::Null => return string("NULL[LOB]"),
            _ => {}
        }
    }
    let type_name = match ctx.types.raw_site(std::slice::from_ref(&value), "sf_probe_text") {
        RawSite::Probe(probe) => return *probe,
        RawSite::Typed(t) => t.into_iter().next().unwrap_or_default(),
        RawSite::Unknown => String::new(),
    };
    let described = match type_name.as_str() {
        "TINYINT" | "UTINYINT" => "NUMBER(38,0)[SB1]".to_owned(),
        "SMALLINT" | "USMALLINT" => "NUMBER(38,0)[SB2]".to_owned(),
        "INTEGER" | "UINTEGER" => "NUMBER(38,0)[SB4]".to_owned(),
        "BIGINT" | "UBIGINT" => "NUMBER(38,0)[SB8]".to_owned(),
        "HUGEINT" | "UHUGEINT" => "NUMBER(38,0)[SB16]".to_owned(),
        t if t.starts_with("DECIMAL(") => {
            let inner = t.trim_start_matches("DECIMAL(").trim_end_matches(')');
            let (width, scale) = inner.split_once(',').unwrap_or((inner, "0"));
            let class = if width.trim().parse::<u32>().unwrap_or(38) > 18 { "SB16" } else { "SB8" };
            format!("NUMBER({},{})[{class}]", width.trim(), scale.trim())
        }
        "FLOAT" | "DOUBLE" => "FLOAT[DOUBLE]".to_owned(),
        "VARCHAR" => "VARCHAR(16777216)[LOB]".to_owned(),
        "BOOLEAN" => "BOOLEAN[SB1]".to_owned(),
        "DATE" => "DATE[SB4]".to_owned(),
        "TIME" => "TIME(9)[SB8]".to_owned(),
        "TIMESTAMP" => "TIMESTAMP_NTZ(9)[SB8]".to_owned(),
        "TIMESTAMPTZ" => "TIMESTAMP_LTZ(9)[SB8]".to_owned(),
        "VARIANT" => "VARIANT[LOB]".to_owned(),
        "NULL" => "NULL[LOB]".to_owned(),
        "" => return func("sf_typeof", vec![value]),
        other if other.ends_with("[]") => "ARRAY[LOB]".to_owned(),
        _ => "OBJECT[LOB]".to_owned(),
    };
    string(described)
}

/// The smallest signed integer storage that holds `n`.
fn storage_class(n: i128) -> &'static str {
    match n {
        n if i8::try_from(n).is_ok() => "SB1",
        n if i16::try_from(n).is_ok() => "SB2",
        n if i32::try_from(n).is_ok() => "SB4",
        n if i64::try_from(n).is_ok() => "SB8",
        _ => "SB16",
    }
}

/// The session's TIMEZONE, as a literal for the engine's conversion functions.
pub fn session_timezone(ctx: &Context) -> Expr {
    string(parameter(ctx, "TIMEZONE"))
}

fn parameter(ctx: &Context, name: &str) -> String {
    ctx.parameters.get(name).cloned().unwrap_or_else(|| {
        crate::DEFAULT_PARAMETERS.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned()).unwrap_or_default()
    })
}

/// Temporary sequences created in every session for `SEQ1`/`SEQ2`/`SEQ4`/`SEQ8`.
/// Each call in a statement uses its own sequence, restarted at 0 first.
pub const SEQUENCES: u32 = 16;

/// SQL helpers for the rewrites above.
pub const MACROS: &str = r#"
CREATE OR REPLACE TEMP MACRO sf_float_text(x) AS
    CASE WHEN isfinite(x) AND x = trunc(x) AND abs(x) < 1e15 THEN CAST(CAST(x AS BIGINT) AS VARCHAR)
        ELSE CAST(x AS VARCHAR) END;
CREATE OR REPLACE TEMP MACRO sf_div_scaled(a, b, m) AS
    CASE WHEN a IS NULL OR b IS NULL THEN NULL
        WHEN b = 0 THEN error('Division by zero')
        WHEN (a * m) // b >= 0 THEN ((a * m) // b + 1) // 2
        ELSE ((a * m) // b - 1) // 2 END;
"#;
