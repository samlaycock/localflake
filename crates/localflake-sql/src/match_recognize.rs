//! `MATCH_RECOGNIZE` as a derived table.
//!
//! 1. `__MR_ROWS`: the input with a row number per partition and a bitmask of
//!    the DEFINE conditions each row satisfies (`PREV`/`NEXT` become LAG/LEAD).
//! 2. `__MR_FOUND` / `__MR_MAP`: per partition, the engine's
//!    `sf_match_recognize` runs the pattern over the bitmasks and returns the
//!    matched rows with their match number and symbol.
//! 3. The matched rows, then MEASURES: aggregated per match for
//!    `ONE ROW PER MATCH`, as running window functions for `ALL ROWS PER MATCH`.
//!
//! DEFINE conditions see the current row (and PREV/NEXT); references to
//! other symbols' rows inside DEFINE are read from the current row too.

use std::collections::HashMap;
use std::ops::ControlFlow;

use sqlparser::ast::{
    AfterMatchSkip, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, MatchRecognizePattern,
    MatchRecognizeSymbol, RepetitionQuantifier, RowsPerMatch, TableAlias, TableFactor, VisitMut, VisitorMut,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

use crate::SqlError;
use crate::ast::quote_literal;

pub fn rewrite(factor: &TableFactor) -> Result<Option<TableFactor>, SqlError> {
    let TableFactor::MatchRecognize {
        table,
        partition_by,
        order_by,
        measures,
        rows_per_match,
        after_match_skip,
        pattern,
        symbols,
        alias,
    } = factor
    else {
        return Ok(None);
    };
    // Symbols in order: DEFINEd ones, then pattern-only ones (always true).
    let mut names: Vec<String> = symbols.iter().map(|s| s.symbol.value.to_uppercase()).collect();
    collect_symbols(pattern, &mut names);
    if names.len() > 63 {
        return Err(SqlError::Unsupported("MATCH_RECOGNIZE with more than 63 symbols".into()));
    }
    let index: HashMap<String, usize> = names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();

    let order = order_by.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let partition = partition_by.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut window = String::new();
    if !partition.is_empty() {
        window.push_str(&format!("PARTITION BY {}", partition.join(", ")));
    }
    if !order.is_empty() {
        window.push_str(&format!(" ORDER BY {order}"));
    }
    let partition_keys: Vec<String> = (0..partition.len()).map(|i| format!("__MR_P{i}")).collect();

    let mut mask_terms = vec![];
    for (i, name) in names.iter().enumerate() {
        let condition = match symbols.iter().find(|s| s.symbol.value.eq_ignore_ascii_case(name)) {
            Some(definition) => {
                let mut condition = definition.definition.clone();
                define_condition(&mut condition, &index, &window);
                condition.to_string()
            }
            None => "TRUE".to_owned(),
        };
        mask_terms.push(format!("CASE WHEN {condition} THEN {} ELSE 0 END", 1_u64 << i));
    }
    let partition_columns: String =
        partition.iter().zip(&partition_keys).map(|(p, k)| format!(", {p} AS {k}")).collect();
    let rows = format!(
        "SELECT *, row_number() OVER ({window}) AS __MR_RN{partition_columns}, ({}) AS __MR_MASK FROM {table}",
        mask_terms.join(" + ")
    );

    let skip = match after_match_skip {
        Some(AfterMatchSkip::ToNextRow) => "TO NEXT ROW",
        _ => "PAST LAST ROW",
    };
    let group_by =
        if partition_keys.is_empty() { String::new() } else { format!(" GROUP BY {}", partition_keys.join(", ")) };
    let key_list: String = partition_keys.iter().map(|k| format!("{k}, ")).collect();
    let found = format!(
        "SELECT {key_list}sf_match_recognize(string_agg(CAST(__MR_MASK AS VARCHAR), ',' ORDER BY __MR_RN), {}, {}) AS __MR_OUT \
         FROM __MR_ROWS{group_by}",
        quote_literal(&serialize(pattern, &index)?),
        quote_literal(skip)
    );
    let map = format!(
        "SELECT {key_list}CAST(split_part(__MR_E, ':', 1) AS BIGINT) AS __MR_RN, CAST(split_part(__MR_E, ':', 2) AS BIGINT) AS __MR_MATCH, \
         CAST(split_part(__MR_E, ':', 3) AS BIGINT) AS __MR_SYM \
         FROM __MR_FOUND, unnest(string_split(__MR_OUT, ';')) AS __MR_U(__MR_E) WHERE __MR_E <> ''"
    );
    let join_keys: String = partition_keys.iter().map(|k| format!(" AND r.{k} IS NOT DISTINCT FROM m.{k}")).collect();
    let classifier = names
        .iter()
        .enumerate()
        .map(|(i, n)| format!("WHEN {i} THEN {}", quote_literal(n)))
        .collect::<Vec<_>>()
        .join(" ");
    let matched = format!(
        "SELECT r.*, m.__MR_MATCH, CASE m.__MR_SYM {classifier} END AS __MR_CLS \
         FROM __MR_ROWS r JOIN __MR_MAP m ON r.__MR_RN = m.__MR_RN{join_keys}"
    );

    let one_row = !matches!(rows_per_match, Some(RowsPerMatch::AllRows(_)));
    let running = format!(
        "OVER (PARTITION BY {}__MR_MATCH ORDER BY __MR_RN ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
        key_list
    );
    let mut columns = vec![];
    for measure in measures {
        let mut expr = measure.expr.clone();
        let mut translator = Measure { symbols: &index, one_row, running: &running };
        let _ = expr.visit(&mut translator);
        columns.push(format!("{expr} AS {}", measure.alias));
    }
    let select = if one_row {
        let keys: Vec<String> = partition_by
            .iter()
            .zip(&partition_keys)
            .map(|(p, k)| format!("{k} AS {}", output_name(p)))
            .chain(columns)
            .collect();
        let group: Vec<String> = partition_keys.iter().cloned().chain(["__MR_MATCH".to_owned()]).collect();
        format!("SELECT {} FROM __MR_MATCHED GROUP BY {}", keys.join(", "), group.join(", "))
    } else {
        let helpers: Vec<String> = ["__MR_RN", "__MR_MASK", "__MR_MATCH", "__MR_CLS"]
            .iter()
            .map(|s| (*s).to_owned())
            .chain(partition_keys.clone())
            .collect();
        let extra: String = columns.iter().map(|c| format!(", {c}")).collect();
        format!("SELECT * EXCLUDE ({}){extra} FROM __MR_MATCHED", helpers.join(", "))
    };
    let sql = format!(
        "WITH __MR_ROWS AS ({rows}), __MR_FOUND AS ({found}), __MR_MAP AS ({map}), __MR_MATCHED AS ({matched}) {select}"
    );
    let subquery = Parser::new(&DuckDbDialect).try_with_sql(&sql)?.parse_query()?;
    Ok(Some(TableFactor::Derived {
        lateral: false,
        subquery,
        alias: alias
            .clone()
            .or_else(|| Some(TableAlias { explicit: true, name: Ident::new("__MR"), columns: vec![], at: None })),
        sample: None,
    }))
}

fn output_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(i) => i.to_string(),
        Expr::CompoundIdentifier(parts) => parts.last().map(ToString::to_string).unwrap_or_default(),
        other => crate::ast::quote(&other.to_string()),
    }
}

fn collect_symbols(pattern: &MatchRecognizePattern, names: &mut Vec<String>) {
    use MatchRecognizePattern::*;
    match pattern {
        Symbol(MatchRecognizeSymbol::Named(id)) | Exclude(MatchRecognizeSymbol::Named(id)) => {
            let name = id.value.to_uppercase();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        Permute(symbols) => symbols.iter().for_each(|s| collect_symbols(&Symbol(s.clone()), names)),
        Concat(items) | Alternation(items) => items.iter().for_each(|p| collect_symbols(p, names)),
        Group(inner) | Repetition(inner, _) => collect_symbols(inner, names),
        _ => {}
    }
}

/// The pattern in the matcher's prefix notation (see the engine's `match_recognize`).
fn serialize(pattern: &MatchRecognizePattern, index: &HashMap<String, usize>) -> Result<String, SqlError> {
    use MatchRecognizePattern::*;
    let symbol = |s: &MatchRecognizeSymbol| match s {
        MatchRecognizeSymbol::Named(id) => format!("#{}", index[&id.value.to_uppercase()]),
        MatchRecognizeSymbol::Start => "^".to_owned(),
        MatchRecognizeSymbol::End => "$".to_owned(),
    };
    Ok(match pattern {
        Symbol(s) | Exclude(s) => symbol(s),
        Permute(symbols) => {
            let items: Vec<String> = symbols.iter().map(symbol).collect();
            let orders: Vec<String> =
                permutations(&items).into_iter().map(|p| format!("(c {})", p.join(" "))).collect();
            format!("(| {})", orders.join(" "))
        }
        Concat(items) => {
            format!("(c {})", items.iter().map(|p| serialize(p, index)).collect::<Result<Vec<_>, _>>()?.join(" "))
        }
        Alternation(items) => {
            format!("(| {})", items.iter().map(|p| serialize(p, index)).collect::<Result<Vec<_>, _>>()?.join(" "))
        }
        Group(inner) => serialize(inner, index)?,
        Repetition(inner, quantifier) => {
            let (min, max): (u32, i64) = match quantifier {
                RepetitionQuantifier::ZeroOrMore => (0, -1),
                RepetitionQuantifier::OneOrMore => (1, -1),
                RepetitionQuantifier::AtMostOne => (0, 1),
                RepetitionQuantifier::Exactly(n) => (*n, i64::from(*n)),
                RepetitionQuantifier::AtLeast(n) => (*n, -1),
                RepetitionQuantifier::AtMost(n) => (0, i64::from(*n)),
                RepetitionQuantifier::Range(a, b) => (*a, i64::from(*b)),
            };
            format!("(r {min} {max} 1 {})", serialize(inner, index)?)
        }
    })
}

fn permutations(items: &[String]) -> Vec<Vec<String>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = vec![];
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first.clone());
            out.push(p);
        }
    }
    out
}

/// DEFINE conditions: PREV/NEXT become LAG/LEAD over the pattern's window and
/// symbol qualifiers are dropped.
fn define_condition(expr: &mut Expr, symbols: &HashMap<String, usize>, window: &str) {
    let _ = sqlparser::ast::visit_expressions_mut(expr, |e| {
        if let Expr::CompoundIdentifier(parts) = e
            && parts.len() == 2
            && symbols.contains_key(&parts[0].value.to_uppercase())
        {
            *e = Expr::Identifier(parts[1].clone());
        }
        if let Expr::Function(f) = e {
            let name = f.name.to_string().to_uppercase();
            if name == "PREV" || name == "NEXT" {
                let args = arg_texts(f);
                let offset = args.get(1).cloned().unwrap_or_else(|| "1".to_owned());
                let shift = if name == "PREV" { "lag" } else { "lead" };
                *e = raw(&format!("{shift}({}, {offset}) OVER ({window})", args.first().cloned().unwrap_or_default()));
            }
        }
        ControlFlow::<()>::Continue(())
    });
}

fn arg_texts(f: &sqlparser::ast::Function) -> Vec<String> {
    match &f.args {
        FunctionArguments::List(list) => list.args.iter().map(ToString::to_string).collect(),
        _ => vec![],
    }
}

/// SQL text spliced into an expression tree.
fn raw(sql: &str) -> Expr {
    Expr::Identifier(Ident::new(sql))
}

/// MEASURES: MATCH_NUMBER(), CLASSIFIER(), FIRST/LAST and aggregates, each
/// over the match's rows (or one symbol's rows when qualified, e.g. `A.price`).
struct Measure<'a> {
    symbols: &'a HashMap<String, usize>,
    one_row: bool,
    running: &'a str,
}

impl Measure<'_> {
    /// The expression with symbol qualifiers removed, and the symbol if any.
    fn unqualify(&self, expr: &Expr) -> (String, Option<String>) {
        let mut symbol = None;
        let mut expr = expr.clone();
        let _ = sqlparser::ast::visit_expressions_mut(&mut expr, |e| {
            if let Expr::CompoundIdentifier(parts) = e
                && parts.len() == 2
                && self.symbols.contains_key(&parts[0].value.to_uppercase())
            {
                symbol = Some(parts[0].value.to_uppercase());
                *e = Expr::Identifier(parts[1].clone());
            }
            ControlFlow::<()>::Continue(())
        });
        (expr.to_string(), symbol)
    }

    fn aggregate(&self, call: &str, symbol: Option<&str>) -> String {
        let filter = symbol.map(|s| format!(" FILTER (WHERE __MR_CLS = {})", quote_literal(s))).unwrap_or_default();
        if self.one_row { format!("{call}{filter}") } else { format!("{call}{filter} {}", self.running) }
    }
}

impl VisitorMut for Measure<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        let replacement = match &*expr {
            Expr::Function(f) => {
                let name = f.name.to_string().to_uppercase();
                let args: Vec<&FunctionArg> = match &f.args {
                    FunctionArguments::List(list) => list.args.iter().collect(),
                    _ => vec![],
                };
                let first_expr = args.first().and_then(|a| match a {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.clone()),
                    _ => None,
                });
                match name.as_str() {
                    "MATCH_NUMBER" => Some("__MR_MATCH".to_owned()),
                    "CLASSIFIER" => {
                        Some(if self.one_row { "arg_max(__MR_CLS, __MR_RN)".to_owned() } else { "__MR_CLS".to_owned() })
                    }
                    "FIRST" | "LAST" => first_expr.map(|e| {
                        let (value, symbol) = self.unqualify(&e);
                        let pick = if name == "FIRST" { "arg_min" } else { "arg_max" };
                        self.aggregate(&format!("{pick}({value}, __MR_RN)"), symbol.as_deref())
                    }),
                    "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "LISTAGG" | "ARRAY_AGG" => {
                        let qualified_star = args.first().and_then(|a| match a {
                            FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(name)) => {
                                Some(name.to_string().to_uppercase())
                            }
                            _ => None,
                        });
                        match (qualified_star, first_expr) {
                            (Some(symbol), _) => Some(self.aggregate("count(*)", Some(&symbol))),
                            (None, Some(e)) => {
                                let (value, symbol) = self.unqualify(&e);
                                Some(self.aggregate(&format!("{}({value})", name.to_lowercase()), symbol.as_deref()))
                            }
                            (None, None) => Some(self.aggregate(&format!("{}(*)", name.to_lowercase()), None)),
                        }
                    }
                    _ => None,
                }
            }
            // A bare column is the value at the last row (of the symbol, if qualified).
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                let (value, symbol) = self.unqualify(expr);
                if self.one_row || symbol.is_some() {
                    Some(self.aggregate(&format!("arg_max({value}, __MR_RN)"), symbol.as_deref()))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(sql) = replacement {
            *expr = raw(&sql);
        }
        ControlFlow::Continue(())
    }
}
