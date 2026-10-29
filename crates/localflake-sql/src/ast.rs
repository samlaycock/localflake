//! Small constructors for building DuckDB-side AST nodes.

use sqlparser::ast::{
    BinaryOperator, CaseWhen, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, Value,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Span;

pub fn func(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name: object_name(&[name]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args.into_iter().map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e))).collect(),
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
    })
}

pub fn object_name(parts: &[&str]) -> ObjectName {
    ObjectName(parts.iter().map(|p| ObjectNamePart::Identifier(Ident::new(*p))).collect())
}

pub fn string(value: impl Into<String>) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.into()).into())
}

pub fn number(value: impl ToString) -> Expr {
    Expr::Value(Value::Number(value.to_string(), false).into())
}

pub fn null() -> Expr {
    Expr::Value(Value::Null.into())
}

pub fn boolean(value: bool) -> Expr {
    Expr::Value(Value::Boolean(value).into())
}

pub fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast { kind: CastKind::Cast, expr: Box::new(expr), data_type, format: None }
}

pub fn try_cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast { kind: CastKind::TryCast, expr: Box::new(expr), data_type, format: None }
}

pub fn custom_type(name: &str) -> DataType {
    DataType::Custom(object_name(&[name]), vec![])
}

pub fn binop(left: Expr, op: BinaryOperator, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(nested(left)), op, right: Box::new(nested(right)) }
}

/// Parenthesizes compound expressions so re-rendering keeps precedence.
pub fn nested(expr: Expr) -> Expr {
    match expr {
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::Value(_)
        | Expr::Function(_)
        | Expr::Nested(_)
        | Expr::Cast { .. }
        | Expr::Case { .. } => expr,
        other => Expr::Nested(Box::new(other)),
    }
}

pub fn case(conditions: Vec<(Expr, Expr)>, else_result: Option<Expr>) -> Expr {
    Expr::Case {
        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
        operand: None,
        conditions: conditions.into_iter().map(|(condition, result)| CaseWhen { condition, result }).collect(),
        else_result: else_result.map(Box::new),
    }
}

pub fn is_null(expr: Expr) -> Expr {
    Expr::IsNull(Box::new(nested(expr)))
}

pub fn is_not_null(expr: Expr) -> Expr {
    Expr::IsNotNull(Box::new(nested(expr)))
}

/// An OBJECT value: stored as JSON, normalized through VARIANT so keys are sorted.
pub fn object_value(expr: Expr) -> Expr {
    cast(cast(expr, custom_type("VARIANT")), custom_type("JSON"))
}

/// An ARRAY value: stored as `VARIANT[]`.
pub fn array_value(expr: Expr) -> Expr {
    cast(cast(expr, custom_type("VARIANT")), custom_type("VARIANT[]"))
}

pub fn and_all(exprs: Vec<Expr>) -> Expr {
    exprs.into_iter().reduce(|a, b| binop(a, BinaryOperator::And, b)).unwrap_or_else(|| boolean(true))
}

pub fn or_all(exprs: Vec<Expr>) -> Expr {
    exprs.into_iter().reduce(|a, b| binop(a, BinaryOperator::Or, b)).unwrap_or_else(|| boolean(false))
}

/// Parses a DuckDB SQL expression. Used for templates that are easier to read as SQL.
pub fn parse_expr(sql: &str) -> Expr {
    Parser::new(&DuckDbDialect).try_with_sql(sql).and_then(|mut p| p.parse_expr()).expect("valid template expression")
}

/// Returns the string value of a literal (`'x'` or `$$x$$`), if `expr` is one.
pub fn string_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Some(s),
            Value::DollarQuotedString(d) => Some(&d.value),
            _ => None,
        },
        _ => None,
    }
}

pub fn integer_literal(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::Number(n, _) => n.parse().ok(),
            _ => None,
        },
        Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Minus, expr } => integer_literal(expr).map(|n| -n),
        _ => None,
    }
}

/// Converts a bare word argument (e.g. the `day` in `DATEADD(day, ...)`) or a string to text.
pub fn word_or_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        _ => string_literal(expr).map(str::to_owned),
    }
}

pub fn ident(value: &str) -> Ident {
    Ident { value: value.to_owned(), quote_style: None, span: Span::empty() }
}

pub fn quoted_ident(value: &str) -> Ident {
    Ident { value: value.to_owned(), quote_style: Some('"'), span: Span::empty() }
}

/// Renders an identifier for DuckDB, always quoted so case is preserved.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// DuckDB keywords that can't be used as bare identifiers, though Snowflake
/// accepts many of them (`at`, `map`, `binary`, `columns`, ...).
const DUCKDB_RESERVED: &[&str] = &[
    "ALL",
    "ANALYSE",
    "ANALYZE",
    "AND",
    "ANTI",
    "ANY",
    "ARRAY",
    "AS",
    "ASC",
    "ASOF",
    "ASYMMETRIC",
    "AT",
    "AUTHORIZATION",
    "BINARY",
    "BOTH",
    "BY",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLLATION",
    "COLUMN",
    "COLUMNS",
    "CONCURRENTLY",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DEFERRABLE",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DO",
    "ELSE",
    "END",
    "EXCEPT",
    "FALSE",
    "FETCH",
    "FOR",
    "FOREIGN",
    "FREEZE",
    "FROM",
    "FULL",
    "GENERATED",
    "GLOB",
    "GROUP",
    "HAVING",
    "ILIKE",
    "IN",
    "INITIALLY",
    "INNER",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "LAMBDA",
    "LATERAL",
    "LEADING",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MAP",
    "NATURAL",
    "NOT",
    "NOTNULL",
    "NULL",
    "OFFSET",
    "ON",
    "ONLY",
    "OR",
    "ORDER",
    "OUTER",
    "OVERLAPS",
    "PIVOT",
    "PIVOT_LONGER",
    "PIVOT_WIDER",
    "PLACING",
    "POSITIONAL",
    "PRIMARY",
    "QUALIFY",
    "REFERENCES",
    "RETURNING",
    "RIGHT",
    "SELECT",
    "SEMI",
    "SHOW",
    "SIMILAR",
    "SOME",
    "STRUCT",
    "SUMMARIZE",
    "SYMMETRIC",
    "TABLE",
    "TABLESAMPLE",
    "THEN",
    "TO",
    "TRAILING",
    "TRUE",
    "TRY_CAST",
    "TUPLE",
    "UNION",
    "UNIQUE",
    "UNPACK",
    "UNPIVOT",
    "USING",
    "VARIADIC",
    "VERBOSE",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

/// Quotes an unquoted identifier that DuckDB reserves.
pub fn duckdb_ident(ident: &mut Ident) {
    if ident.quote_style.is_none() && DUCKDB_RESERVED.contains(&ident.value.to_uppercase().as_str()) {
        ident.quote_style = Some('"');
    }
}
