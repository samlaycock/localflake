//! The AST rewrite from Snowflake semantics to DuckDB.

use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, CastKind, CeilFloorKind, DataType, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    LimitClause, ObjectName, ObjectNamePart, Query, Select, SelectItem, SetExpr, SetOperator, Statement, TableAlias,
    TableAliasColumnDef, TableFactor, TimezoneInfo, TopQuantity, UnaryOperator, Value, VisitMut, VisitorMut,
};

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::parser::Parser;

use crate::ast::*;
use crate::datetime::Part;
use crate::{Context, SqlError, functions, information_schema, types, variant};

/// Rewrites a parsed statement in place. Returns the query IDs referenced by
/// `RESULT_SCAN`, which the engine must materialize before running the SQL.
pub fn rewrite_statement(statement: &mut Statement, ctx: &Context, source: &str) -> Result<Vec<String>, SqlError> {
    if let ControlFlow::Break(err) = statement.visit(&mut Namer { source }) {
        return Err(err);
    }
    let mut rewriter = Rewriter { ctx, result_scans: Vec::new() };
    match statement.visit(&mut rewriter) {
        ControlFlow::Break(err) => Err(err),
        ControlFlow::Continue(()) => Ok(rewriter.result_scans),
    }
}

/// Rewrites a standalone expression (e.g. a column default or `SET` value).
pub fn rewrite_expr(expr: &mut Expr, ctx: &Context) -> Result<(), SqlError> {
    let mut rewriter = Rewriter { ctx, result_scans: Vec::new() };
    match expr.visit(&mut rewriter) {
        ControlFlow::Break(err) => Err(err),
        ControlFlow::Continue(()) => Ok(()),
    }
}

/// Snowflake names an unaliased expression column after its source text in
/// upper case (`select a+1` → `A+1`), where DuckDB would name it `(A + 1)`.
struct Namer<'a> {
    source: &'a str,
}

impl VisitorMut for Namer<'_> {
    type Break = SqlError;

    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<SqlError> {
        let texts = projection_texts(self.source, select);
        for (i, item) in select.projection.iter_mut().enumerate() {
            if let SelectItem::UnnamedExpr(expr) = item {
                if matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
                    continue;
                }
                let text = texts.as_ref().and_then(|t| t.get(i).cloned()).unwrap_or_else(|| expr.to_string());
                *item = SelectItem::ExprWithAlias { expr: expr.clone(), alias: quoted_ident(&text.to_uppercase()) };
            }
        }
        for item in &mut select.projection {
            if let SelectItem::ExprWithAlias { alias, .. } = item {
                duckdb_ident(alias);
            }
        }
        for table in &mut select.from {
            for join in &mut table.joins {
                if let Err(err) = asof_to_lateral(join) {
                    return ControlFlow::Break(err);
                }
            }
        }
        object_construct_star(select);
        if let Err(err) = conditional_change_event(select) {
            return ControlFlow::Break(err);
        }
        if let Err(err) = sequences_in_windows(select) {
            return ControlFlow::Break(err);
        }
        ControlFlow::Continue(())
    }
}

/// The source text of each item in a SELECT list, found by splitting the
/// tokens after the `SELECT` keyword on top-level commas.
fn projection_texts(source: &str, select: &Select) -> Option<Vec<String>> {
    use sqlparser::tokenizer::{Token, Tokenizer};
    let start = select.select_token.0.span.start;
    if start.line == 0 {
        return None;
    }
    let offsets = crate::normalize::LineOffsets::new(source);
    let begin = offsets.byte_offset(source, start.line, start.column);
    let tail = source.get(begin..)?;
    let tokens = Tokenizer::new(&sqlparser::dialect::SnowflakeDialect, tail).tokenize_with_location().ok()?;
    let tail_offsets = crate::normalize::LineOffsets::new(tail);
    let at = |t: &sqlparser::tokenizer::TokenWithSpan| {
        tail_offsets.byte_offset(tail, t.span.start.line, t.span.start.column)
    };

    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut item_start: Option<usize> = None;
    let mut iter = tokens.iter().skip(1).peekable();
    // Skip DISTINCT / ALL / TOP n.
    while let Some(t) = iter.peek() {
        match &t.token {
            Token::Whitespace(_) => {}
            Token::Word(w)
                if w.quote_style.is_none() && matches!(w.value.to_uppercase().as_str(), "DISTINCT" | "ALL") => {}
            Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case("TOP") => {
                iter.next();
                while matches!(iter.peek().map(|t| &t.token), Some(Token::Whitespace(_))) {
                    iter.next();
                }
            }
            _ => break,
        }
        iter.next();
    }
    let mut end = tail.len();
    for t in iter {
        match &t.token {
            Token::Whitespace(_) => continue,
            Token::LParen | Token::LBracket | Token::LBrace => depth += 1,
            Token::RParen | Token::RBracket | Token::RBrace if depth == 0 => {
                end = at(t);
                break;
            }
            Token::RParen | Token::RBracket | Token::RBrace => depth -= 1,
            Token::Comma if depth == 0 => {
                items.push(tail[item_start?..at(t)].trim().to_owned());
                item_start = None;
                continue;
            }
            Token::SemiColon => {
                end = at(t);
                break;
            }
            Token::Word(w)
                if depth == 0
                    && w.quote_style.is_none()
                    && matches!(
                        w.value.to_uppercase().as_str(),
                        "FROM"
                            | "WHERE"
                            | "GROUP"
                            | "HAVING"
                            | "QUALIFY"
                            | "ORDER"
                            | "LIMIT"
                            | "UNION"
                            | "EXCEPT"
                            | "INTERSECT"
                            | "MINUS"
                            | "WINDOW"
                            | "FETCH"
                            | "OFFSET"
                            | "INTO"
                    ) =>
            {
                end = at(t);
                break;
            }
            _ => {}
        }
        item_start.get_or_insert(at(t));
    }
    if let Some(s) = item_start {
        items.push(tail[s..end].trim().to_owned());
    }
    (items.len() == select.projection.len()).then_some(items)
}

struct Rewriter<'a> {
    ctx: &'a Context,
    result_scans: Vec<String>,
}

/// `CONDITIONAL_CHANGE_EVENT(e) OVER (w)`: the number of times `e` changed
/// (ignoring NULLs) up to each row. DuckDB can't nest window functions, so a
/// subquery computes a per-row change flag with LAG and the call becomes a
/// running SUM of it. The WHERE clause moves into the subquery, which keeps
/// the relation's alias so other references still resolve.
fn conditional_change_event(select: &mut Select) -> Result<(), SqlError> {
    use sqlparser::ast::WindowType;
    let is_cce = |e: &Expr| matches!(e, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("CONDITIONAL_CHANGE_EVENT"));
    let mut found = false;
    let _ = sqlparser::ast::visit_expressions(&select.projection, |e| {
        found |= is_cce(e);
        ControlFlow::<()>::Continue(())
    });
    if !found {
        return Ok(());
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(SqlError::Unsupported("CONDITIONAL_CHANGE_EVENT over joins".into()));
    }
    let alias = match &select.from[0].relation {
        TableFactor::Table { alias: Some(a), .. } | TableFactor::Derived { alias: Some(a), .. } => a.name.value.clone(),
        TableFactor::Table { name, .. } => name.0.last().map(|p| p.to_string()).unwrap_or_default(),
        _ => "__CCE".to_owned(),
    };
    let mut flags = vec![];
    let mut failure = None;
    let _ = sqlparser::ast::visit_expressions_mut(&mut select.projection, |e| {
        if is_cce(e) {
            let Expr::Function(f) = e else { unreachable!() };
            let (Some(WindowType::WindowSpec(spec)), FunctionArguments::List(list)) = (&f.over, &f.args) else {
                failure = Some(SqlError::Unsupported("CONDITIONAL_CHANGE_EVENT without a window".into()));
                return ControlFlow::Break(());
            };
            let value = list.args.first().map(ToString::to_string).unwrap_or_default();
            let i = flags.len();
            let previous = format!("lag({value}) IGNORE NULLS OVER ({spec})");
            flags.push(format!(
                "CASE WHEN ({value}) IS NOT NULL AND {previous} IS NOT NULL AND ({value}) IS DISTINCT FROM {previous} THEN 1 ELSE 0 END AS __CCE{i}"
            ));
            let mut running = spec.clone();
            running.window_frame = Some(sqlparser::ast::WindowFrame {
                units: sqlparser::ast::WindowFrameUnits::Rows,
                start_bound: sqlparser::ast::WindowFrameBound::Preceding(None),
                end_bound: Some(sqlparser::ast::WindowFrameBound::CurrentRow),
            });
            *e = Parser::new(&SnowflakeDialect)
                .try_with_sql(&format!("sum(__CCE{i}) OVER ({running})"))
                .and_then(|mut p| p.parse_expr())
                .expect("valid window expression");
        }
        ControlFlow::Continue(())
    });
    if let Some(err) = failure {
        return Err(err);
    }
    let mut relation = select.from[0].relation.clone();
    if let TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } = &mut relation {
        *alias = None;
    }
    let filter = select.selection.take().map(|w| format!(" WHERE {w}")).unwrap_or_default();
    let sql = format!("SELECT {alias}.*, {} FROM {relation} AS {alias}{filter}", flags.join(", "));
    let subquery = Parser::new(&SnowflakeDialect).try_with_sql(&sql)?.parse_query()?;
    select.from[0].relation = TableFactor::Derived {
        lateral: false,
        subquery,
        alias: Some(TableAlias { explicit: true, name: Ident::new(alias), columns: vec![], at: None }),
        sample: None,
    };
    Ok(())
}

/// `SEQ4()` (etc.) inside a window's PARTITION BY or ORDER BY: SEQ is
/// volatile, which DuckDB rejects there, so the values come from a subquery
/// column instead (single-relation FROM only, like CONDITIONAL_CHANGE_EVENT).
fn sequences_in_windows(select: &mut Select) -> Result<(), SqlError> {
    use sqlparser::ast::WindowType;
    let is_seq = |e: &Expr| matches!(e, Expr::Function(f) if matches!(f.name.to_string().to_uppercase().as_str(), "SEQ1" | "SEQ2" | "SEQ4" | "SEQ8"));
    let mut calls = vec![];
    let _ = sqlparser::ast::visit_expressions_mut(&mut select.projection, |e| {
        if let Expr::Function(f) = e
            && let Some(WindowType::WindowSpec(spec)) = &mut f.over
        {
            let mut replace = |inner: &mut Expr| {
                let _ = sqlparser::ast::visit_expressions_mut(inner, |x| {
                    if is_seq(x) {
                        let name = format!("__SEQ{}", calls.len());
                        calls.push(format!("{x} AS {name}"));
                        *x = Expr::Identifier(Ident::new(name));
                    }
                    ControlFlow::<()>::Continue(())
                });
            };
            spec.partition_by.iter_mut().for_each(&mut replace);
            spec.order_by.iter_mut().for_each(|o| replace(&mut o.expr));
        }
        ControlFlow::<()>::Continue(())
    });
    if calls.is_empty() {
        return Ok(());
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(SqlError::Unsupported("SEQ functions in window definitions over joins".into()));
    }
    let alias = match &select.from[0].relation {
        TableFactor::Table { alias: Some(a), .. } | TableFactor::Derived { alias: Some(a), .. } => a.name.value.clone(),
        TableFactor::Table { name, .. } => name.0.last().map(|p| p.to_string()).unwrap_or_default(),
        _ => "__SEQ".to_owned(),
    };
    let mut relation = select.from[0].relation.clone();
    match &mut relation {
        TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } => *alias = None,
        TableFactor::Function { alias, .. } | TableFactor::TableFunction { alias, .. } => *alias = None,
        _ => {}
    }
    let filter = select.selection.take().map(|w| format!(" WHERE {w}")).unwrap_or_default();
    let sql = format!("SELECT {alias}.*, {} FROM {relation} AS {alias}{filter}", calls.join(", "));
    let subquery = Parser::new(&SnowflakeDialect).try_with_sql(&sql)?.parse_query()?;
    select.from[0].relation = TableFactor::Derived {
        lateral: false,
        subquery,
        alias: Some(TableAlias { explicit: true, name: Ident::new(alias), columns: vec![], at: None }),
        sample: None,
    };
    Ok(())
}

/// `OBJECT_CONSTRUCT(*)` over a single relation: the row as a JSON object,
/// via DuckDB's `to_json(<relation alias>)`.
fn object_construct_star(select: &mut Select) {
    let is_star_call = |e: &Expr| {
        matches!(e, Expr::Function(f)
            if matches!(f.name.to_string().to_uppercase().as_str(), "OBJECT_CONSTRUCT" | "OBJECT_CONSTRUCT_KEEP_NULL")
            && matches!(&f.args, FunctionArguments::List(l) if matches!(l.args.as_slice(), [FunctionArg::Unnamed(FunctionArgExpr::Wildcard)])))
    };
    let mut found = false;
    let _ = sqlparser::ast::visit_expressions(&select.projection, |e| {
        found |= is_star_call(e);
        ControlFlow::<()>::Continue(())
    });
    if !found || select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return;
    }
    let alias = match &mut select.from[0].relation {
        TableFactor::Table { alias: Some(a), .. } | TableFactor::Derived { alias: Some(a), .. } => a.name.clone(),
        TableFactor::Table { name, .. } => match name.0.last() {
            Some(ObjectNamePart::Identifier(i)) => i.clone(),
            _ => return,
        },
        TableFactor::Derived { alias, .. } => {
            let name = Ident::new("__OC");
            *alias = Some(TableAlias { explicit: true, name: name.clone(), columns: vec![], at: None });
            name
        }
        _ => return,
    };
    let _ = sqlparser::ast::visit_expressions_mut(&mut select.projection, |e| {
        if is_star_call(e) {
            let keep_null = matches!(e, Expr::Function(f) if f.name.to_string().to_uppercase().ends_with("KEEP_NULL"));
            let object = func("to_json", vec![Expr::Identifier(alias.clone())]);
            let object = if keep_null { object } else { func("json_merge_patch", vec![string("{}"), object]) };
            *e = object_value(object);
        }
        ControlFlow::<()>::Continue(())
    });
}

/// `SAMPLE (p)` / `SAMPLE (n ROWS)` → DuckDB's `TABLESAMPLE`, which goes after the alias.
fn translate_sample(sample: &mut Option<sqlparser::ast::TableSampleKind>) {
    use sqlparser::ast::{TableSampleKind, TableSampleMethod, TableSampleModifier, TableSampleUnit};
    let Some(kind) = sample.take() else { return };
    let (TableSampleKind::BeforeTableAlias(mut s) | TableSampleKind::AfterTableAlias(mut s)) = kind;
    s.modifier = TableSampleModifier::TableSample;
    if let Some(quantity) = &mut s.quantity {
        quantity.parenthesized = true;
        if quantity.unit == Some(TableSampleUnit::Rows) {
            s.name = None;
        } else {
            quantity.unit = Some(TableSampleUnit::Percent);
            s.name = Some(match s.name {
                Some(TableSampleMethod::System | TableSampleMethod::Block) => TableSampleMethod::System,
                _ => TableSampleMethod::Bernoulli,
            });
        }
    }
    *sample = Some(TableSampleKind::AfterTableAlias(s));
}

/// `ASOF JOIN r MATCH_CONDITION (l.t >= r.t) ON l.k = r.k` as
/// `LEFT JOIN LATERAL (SELECT * FROM r WHERE ... ORDER BY r.t DESC LIMIT 1) r ON TRUE`:
/// for each left row, the closest right row that satisfies the condition.
fn asof_to_lateral(join: &mut sqlparser::ast::Join) -> Result<(), SqlError> {
    use sqlparser::ast::{JoinConstraint, JoinOperator};
    let JoinOperator::AsOf { match_condition, constraint } = &join.join_operator else { return Ok(()) };
    let Expr::BinaryOp { left, op, right } = match_condition else {
        return Err(SqlError::Unsupported("ASOF JOIN with a non-comparison MATCH_CONDITION".into()));
    };
    let alias = match &join.relation {
        TableFactor::Table { alias: Some(a), .. } | TableFactor::Derived { alias: Some(a), .. } => a.name.value.clone(),
        TableFactor::Table { name, .. } => name.0.last().map(|p| p.to_string()).unwrap_or_default(),
        _ => return Err(SqlError::Unsupported("ASOF JOIN on this relation".into())),
    };
    let belongs_right = |e: &Expr| matches!(e, Expr::CompoundIdentifier(parts) if parts.len() > 1 && parts[0].value.eq_ignore_ascii_case(&alias));
    // The right-hand column, and whether the closest match is its largest value.
    let (right_col, descending) = match op {
        BinaryOperator::GtEq | BinaryOperator::Gt if belongs_right(left) => (left, false),
        BinaryOperator::GtEq | BinaryOperator::Gt => (right, true),
        BinaryOperator::LtEq | BinaryOperator::Lt if belongs_right(left) => (left, true),
        BinaryOperator::LtEq | BinaryOperator::Lt => (right, false),
        _ => return Err(SqlError::Unsupported("ASOF JOIN MATCH_CONDITION operator".into())),
    };
    let mut filter = match_condition.to_string();
    if let JoinConstraint::On(on) = constraint {
        filter = format!("({on}) AND ({filter})");
    }
    let mut relation = join.relation.clone();
    let relation_alias = match &mut relation {
        TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } => alias.take(),
        _ => None,
    };
    let order = if descending { "DESC" } else { "ASC" };
    let sql =
        format!("SELECT * FROM {relation} AS {alias} WHERE {filter} ORDER BY {right_col} {order} NULLS LAST LIMIT 1");
    let subquery = Parser::new(&SnowflakeDialect).try_with_sql(&sql)?.parse_query()?;
    join.relation = TableFactor::Derived {
        lateral: true,
        subquery,
        alias: relation_alias
            .or_else(|| Some(TableAlias { explicit: true, name: Ident::new(alias), columns: vec![], at: None })),
        sample: None,
    };
    join.join_operator = JoinOperator::LeftOuter(JoinConstraint::On(Expr::Value(Value::Boolean(true).into())));
    Ok(())
}

/// Snowflake names the columns of `FROM VALUES (...)` `COLUMN1`, `COLUMN2`, ...;
/// DuckDB would name them `col0`, `col1`, ...
fn name_values_columns(factor: &mut TableFactor) {
    let TableFactor::Derived { subquery, alias, .. } = factor else { return };
    let SetExpr::Values(values) = subquery.body.as_ref() else { return };
    let Some(width) = values.rows.first().map(|row| row.content.len()) else { return };
    let alias = alias.get_or_insert_with(|| TableAlias {
        explicit: false,
        name: Ident::new("__VALUES"),
        columns: vec![],
        at: None,
    });
    if alias.columns.is_empty() {
        alias.columns = (1..=width).map(|i| TableAliasColumnDef::from_name(format!("COLUMN{i}"))).collect();
    }
}

impl VisitorMut for Rewriter<'_> {
    type Break = SqlError;

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<SqlError> {
        if let Err(err) = crate::connect_by::rewrite(query) {
            return ControlFlow::Break(err);
        }
        // `SELECT TOP n` → `LIMIT n`.
        if let SetExpr::Select(select) = query.body.as_mut()
            && let Some(top) = select.top.take()
        {
            let quantity = match top.quantity {
                Some(TopQuantity::Expr(e)) => e,
                Some(TopQuantity::Constant(n)) => number(n),
                None => return ControlFlow::Continue(()),
            };
            if query.limit_clause.is_none() {
                query.limit_clause =
                    Some(LimitClause::LimitOffset { limit: Some(quantity), offset: None, limit_by: vec![] });
            }
        }
        minus_to_except(query.body.as_mut());
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<SqlError> {
        // `IDENTIFIER('db.schema.table')` as a table name.
        if let [ObjectNamePart::Function(f)] = relation.0.as_slice()
            && f.name.value.eq_ignore_ascii_case("IDENTIFIER")
        {
            let literal = f.args.first().and_then(|a| match a {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => string_literal(e).map(str::to_owned),
                _ => None,
            });
            match literal {
                Some(name) => *relation = parse_object_name(&name),
                None => {
                    return ControlFlow::Break(SqlError::Unsupported("IDENTIFIER with a non-literal argument".into()));
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<SqlError> {
        match statement {
            Statement::Insert(insert) => {
                for name in &mut insert.columns {
                    name.0.iter_mut().for_each(|p| {
                        if let ObjectNamePart::Identifier(i) = p {
                            duckdb_ident(i)
                        }
                    });
                }
            }
            Statement::Update(update) => {
                for assignment in &mut update.assignments {
                    if let sqlparser::ast::AssignmentTarget::ColumnName(name) = &mut assignment.target {
                        name.0.iter_mut().for_each(|p| {
                            if let ObjectNamePart::Identifier(i) = p {
                                duckdb_ident(i)
                            }
                        });
                    }
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<SqlError> {
        match crate::match_recognize::rewrite(factor) {
            Ok(Some(replacement)) => {
                *factor = replacement;
                return ControlFlow::Continue(());
            }
            Ok(None) => {}
            Err(err) => return ControlFlow::Break(err),
        }
        if let TableFactor::Table { alias: Some(a), .. } | TableFactor::Derived { alias: Some(a), .. } = factor {
            duckdb_ident(&mut a.name);
            a.columns.iter_mut().for_each(|c| duckdb_ident(&mut c.name));
        }
        name_values_columns(factor);
        if let TableFactor::Table { sample, .. } | TableFactor::Derived { sample, .. } = factor {
            translate_sample(sample);
        }
        match self.table_factor(factor) {
            Ok(Some(replacement)) => *factor = replacement,
            Ok(None) => {}
            Err(err) => return ControlFlow::Break(err),
        }
        ControlFlow::Continue(())
    }

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<SqlError> {
        match expr {
            // A bare DEFAULT (`SET x = DEFAULT`) is the keyword, not a column.
            Expr::Identifier(i) if !i.value.eq_ignore_ascii_case("DEFAULT") => duckdb_ident(i),
            Expr::CompoundIdentifier(parts) => parts.iter_mut().for_each(duckdb_ident),
            _ => {}
        }
        match self.expr(expr) {
            Ok(Some(replacement)) => *expr = replacement,
            Ok(None) => {}
            Err(err) => return ControlFlow::Break(err),
        }
        ControlFlow::Continue(())
    }
}

impl Rewriter<'_> {
    fn expr(&mut self, expr: &mut Expr) -> Result<Option<Expr>, SqlError> {
        let is_ceil = matches!(expr, Expr::Ceil { .. });
        Ok(Some(match expr {
            Expr::JsonAccess { value, path } => {
                variant::path_access(take(value), sqlparser::ast::JsonPath { path: std::mem::take(&mut path.path) })
            }
            Expr::BinaryOp { left, op: BinaryOperator::Divide, right } => {
                crate::typing::divide(&self.ctx.types, take(left), take(right))
            }
            Expr::Cast { kind, expr: inner, data_type, .. } => {
                return self.cast(kind.clone(), take(inner), data_type).map(Some);
            }
            Expr::TypedString(typed) => {
                let target = types::map_type(&typed.data_type, self.ctx)?;
                cast(Expr::Value(typed.value.clone()), target)
            }
            Expr::Function(f) => return functions::rewrite(f, self.ctx),
            // sqlparser parses SUBSTR/SUBSTRING into their own node; rewrite them as calls.
            Expr::Substring { expr: value, substring_from, substring_for, .. } => {
                let args = std::iter::once(take(value))
                    .chain(substring_from.take().map(|e| *e))
                    .chain(substring_for.take().map(|e| *e))
                    .collect();
                let Expr::Function(mut call) = func("SUBSTR", args) else { unreachable!() };
                return Ok(Some(functions::rewrite(&mut call, self.ctx)?.unwrap_or(Expr::Function(call))));
            }
            Expr::Identifier(ident) if ident.quote_style.is_none() && is_niladic(&ident.value) => {
                let mut call = match func(&ident.value, vec![]) {
                    Expr::Function(f) => f,
                    _ => unreachable!(),
                };
                call.args = FunctionArguments::None;
                return Ok(Some(functions::rewrite(&mut call, self.ctx)?.unwrap_or(Expr::Function(call))));
            }
            Expr::CompoundIdentifier(parts)
                if parts.last().is_some_and(|p| p.value == "NEXTVAL") && parts.len() > 1 =>
            {
                let sequence: Vec<_> = parts[..parts.len() - 1].iter().map(|p| quote(&p.value)).collect();
                func("nextval", vec![string(sequence.join("."))])
            }
            Expr::RLike { negated, expr: subject, pattern, .. } => {
                let matched = func("regexp_full_match", vec![take(subject), take(pattern)]);
                if *negated {
                    Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Not, expr: Box::new(matched) }
                } else {
                    matched
                }
            }
            Expr::Like { negated, any: true, expr: subject, pattern, escape_char } => {
                return Ok(Some(like_any(take(subject), take(pattern), *negated, false, escape_char.take(), false)));
            }
            Expr::ILike { negated, any: true, expr: subject, pattern, escape_char } => {
                return Ok(Some(like_any(take(subject), take(pattern), *negated, true, escape_char.take(), false)));
            }
            // `x LIKE ALL (a, b)` parses as a LIKE whose pattern is a call to `all`.
            Expr::Like { negated, any: false, expr: subject, pattern, escape_char }
                if all_patterns(pattern).is_some() =>
            {
                let patterns = Expr::Tuple(all_patterns(pattern).unwrap());
                return Ok(Some(like_any(take(subject), patterns, *negated, false, escape_char.take(), true)));
            }
            Expr::ILike { negated, any: false, expr: subject, pattern, escape_char }
                if all_patterns(pattern).is_some() =>
            {
                let patterns = Expr::Tuple(all_patterns(pattern).unwrap());
                return Ok(Some(like_any(take(subject), patterns, *negated, true, escape_char.take(), true)));
            }
            Expr::Extract { field, expr: inner, .. } => {
                let name = field.to_string();
                match Part::parse(&name) {
                    Some(part) => functions::date_part(part, take(inner)),
                    None => func("date_part", vec![string(name.to_lowercase()), take(inner)]),
                }
            }
            Expr::Ceil { expr: inner, field: CeilFloorKind::Scale(scale) }
            | Expr::Floor { expr: inner, field: CeilFloorKind::Scale(scale) } => {
                let name = if is_ceil { "ceil" } else { "floor" };
                let Value::Number(digits, _) = &scale.value else {
                    return Err(SqlError::Unsupported(format!("{} with a non-literal scale", name.to_uppercase())));
                };
                let digits: i32 = digits.parse().map_err(|_| SqlError::Arguments(name.to_uppercase()))?;
                let factor = number(10_f64.powi(digits));
                let scaled = func(name, vec![binop(take(inner), BinaryOperator::Multiply, factor.clone())]);
                let precision = sqlparser::ast::ExactNumberInfo::PrecisionAndScale(38, i64::from(digits.max(0)));
                cast(binop(scaled, BinaryOperator::Divide, factor), DataType::Decimal(precision))
            }
            // DATE ± INTERVAL stays a DATE when the interval only has date parts.
            Expr::BinaryOp { left, op: op @ (BinaryOperator::Plus | BinaryOperator::Minus), right } if matches!(right.as_ref(), Expr::Interval(i) if date_only_interval(i)) =>
            {
                let left = take(left);
                let sum = binop(left.clone(), op.clone(), take(right));
                func("sf_cast_like", vec![sum, left])
            }
            Expr::Value(v) => match &v.value {
                Value::Placeholder(p) => return self.placeholder(p).map(Some),
                _ => return Ok(None),
            },
            Expr::Array(array) => {
                let items = std::mem::take(&mut array.elem);
                array_value(if items.is_empty() { variant::empty_array() } else { func("json_array", items) })
            }
            Expr::Dictionary(fields) => {
                let args = std::mem::take(fields)
                    .into_iter()
                    .flat_map(|f| [string(f.key.value), *f.value])
                    .collect::<Vec<_>>();
                object_value(if args.is_empty() { variant::empty_object() } else { func("json_object", args) })
            }
            // `a || b` converts non-text operands the way Snowflake prints them.
            Expr::BinaryOp { left, op: BinaryOperator::StringConcat, right } => {
                let (left, right) = (take(left), take(right));
                binop(
                    crate::typing::text(self.ctx, left),
                    BinaryOperator::StringConcat,
                    crate::typing::text(self.ctx, right),
                )
            }
            // A numeric string literal in arithmetic is a number: '5' + 1 is 6.
            Expr::BinaryOp {
                left,
                op:
                    op @ (BinaryOperator::Plus | BinaryOperator::Minus | BinaryOperator::Multiply | BinaryOperator::Modulo),
                right,
            } if numeric_literal(left).is_some() || numeric_literal(right).is_some() => {
                let side = |e: &mut Box<Expr>| numeric_literal(e).unwrap_or_else(|| take(e));
                binop(side(left), op.clone(), side(right))
            }
            Expr::UnaryOp { op: UnaryOperator::Minus, expr: inner } if numeric_literal(inner).is_some() => {
                Expr::UnaryOp { op: UnaryOperator::Minus, expr: Box::new(numeric_literal(inner).unwrap()) }
            }
            Expr::Collate { expr: inner, collation } => {
                let spec = Expr::Value(
                    Value::SingleQuotedString(collation.to_string().trim_matches(['\'', '"']).to_owned()).into(),
                );
                Expr::Collate { expr: Box::new(take(inner)), collation: functions::collation_name(&spec)? }
            }
            Expr::Lambda(lambda) => {
                lambda.syntax = sqlparser::ast::LambdaSyntax::LambdaKeyword;
                return Ok(None);
            }
            _ => return Ok(None),
        }))
    }

    fn cast(&self, kind: CastKind, inner: Expr, data_type: &DataType) -> Result<Expr, SqlError> {
        let try_ = matches!(kind, CastKind::TryCast | CastKind::SafeCast);
        let wrap = |e: Expr| if try_ { func("try", vec![e]) } else { e };
        if types::is_text(data_type) {
            return Ok(wrap(crate::typing::text(self.ctx, inner)));
        }
        let target = types::map_type(data_type, self.ctx)?;
        // VARIANT inputs convert through their text, e.g. '"2024-01-01"'::DATE.
        let convert = |name: &str, inner: Expr| {
            let is_variant = binop(func("typeof", vec![inner.clone()]), BinaryOperator::Eq, string("VARIANT"));
            // Conversions to NTZ and DATE see TIMESTAMP_LTZ in the session's time zone.
            let zone =
                matches!(name, "sf_to_timestamp" | "sf_to_date").then(|| crate::typing::session_timezone(self.ctx));
            let call = |arg: Expr| func(name, std::iter::once(arg).chain(zone.clone()).collect());
            let from_text = call(cast(inner.clone(), DataType::Varchar(None)));
            wrap(case(vec![(is_variant, from_text)], Some(call(inner))))
        };
        let semi_structured = matches!(&target, DataType::Custom(name, _) if matches!(name.to_string().as_str(), "VARIANT" | "JSON" | "VARIANT[]"));
        if semi_structured {
            return Ok(wrap(match target.to_string().as_str() {
                "JSON" => object_value(inner),
                "VARIANT[]" => array_value(inner),
                _ => cast(inner, target),
            }));
        }
        let json_null = func("sf_is_json_null", vec![inner.clone()]);
        let is_timestamp_tz = matches!(&target, DataType::Custom(name, _) if name.to_string() == "TIMESTAMP_NS");
        let converted = match &target {
            _ if is_timestamp_tz => wrap(crate::typing::to_timestamp_tz(self.ctx, inner)),
            DataType::Timestamp(_, TimezoneInfo::None) => convert("sf_to_timestamp", inner),
            DataType::Timestamp(_, _) => convert("sf_ltz", inner),
            DataType::Date => convert("sf_to_date", inner),
            DataType::Boolean => convert("sf_to_boolean", inner),
            _ => Expr::Cast {
                kind: if try_ { CastKind::TryCast } else { CastKind::Cast },
                expr: Box::new(inner),
                data_type: target,
                format: None,
            },
        };
        Ok(case(vec![(json_null, null())], Some(converted)))
    }

    /// `:1` binds become DuckDB's `$1`; `$name` reads a session variable.
    fn placeholder(&self, placeholder: &str) -> Result<Expr, SqlError> {
        if let Some(n) = placeholder.strip_prefix(':').filter(|n| n.chars().all(|c| c.is_ascii_digit())) {
            return Ok(Expr::Value(Value::Placeholder(format!("${n}")).into()));
        }
        if let Some(name) = placeholder.strip_prefix('$').filter(|n| !n.chars().all(|c| c.is_ascii_digit())) {
            let name = name.trim_matches('"').to_uppercase();
            return match self.ctx.variables.get(&name) {
                Some(value) => Ok(parse_expr(value)),
                None => Err(SqlError::UnknownVariable(name)),
            };
        }
        // `$1`, `$2`, ... are positional columns, as in `SELECT $1 FROM VALUES (...)`.
        if let Some(n) = placeholder.strip_prefix('$').filter(|n| n.chars().all(|c| c.is_ascii_digit())) {
            return Ok(Expr::Identifier(Ident::new(format!("COLUMN{n}"))));
        }
        Ok(Expr::Value(Value::Placeholder(placeholder.to_owned()).into()))
    }

    fn table_factor(&mut self, factor: &mut TableFactor) -> Result<Option<TableFactor>, SqlError> {
        let (name, args, alias) = match factor {
            TableFactor::Function { name, args, alias, .. } => {
                (name.to_string().to_uppercase(), args.clone(), alias.take())
            }
            TableFactor::TableFunction { expr: Expr::Function(f), alias } => {
                let args = match &f.args {
                    FunctionArguments::List(list) => list.args.clone(),
                    _ => vec![],
                };
                (f.name.to_string().to_uppercase(), args, alias.take())
            }
            TableFactor::Table { name, alias, args: None, .. } => {
                let parts: Vec<String> = name.0.iter().map(|p| p.to_string().trim_matches('"').to_owned()).collect();
                if let [.., schema, view] = parts.as_slice()
                    && schema.eq_ignore_ascii_case("INFORMATION_SCHEMA")
                {
                    let database = match parts.len() {
                        3 => parts[0].clone(),
                        _ => self.ctx.database.clone().ok_or(SqlError::NoDatabase)?,
                    };
                    let sql = information_schema::view(&database, &view.to_uppercase())
                        .ok_or_else(|| SqlError::Unsupported(format!("INFORMATION_SCHEMA.{view}")))?;
                    let alias = alias.take().or_else(|| Some(table_alias(view)));
                    let subquery = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect)
                        .try_with_sql(&sql)?
                        .parse_query()?;
                    return Ok(Some(TableFactor::Derived { lateral: false, subquery, alias, sample: None }));
                }
                return Ok(None);
            }
            _ => return Ok(None),
        };
        let named = |key: &str, position: Option<usize>| -> Option<Expr> {
            args.iter()
                .find_map(|a| match a {
                    FunctionArg::Named { name, arg: FunctionArgExpr::Expr(e), .. }
                        if name.value.eq_ignore_ascii_case(key) =>
                    {
                        Some(e.clone())
                    }
                    _ => None,
                })
                .or_else(|| {
                    let positional: Vec<_> = args
                        .iter()
                        .filter_map(|a| match a {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.clone()),
                            _ => None,
                        })
                        .collect();
                    position.and_then(|i| positional.get(i).cloned())
                })
        };
        let restore_alias = |factor: &mut TableFactor, alias| match factor {
            TableFactor::Function { alias: a, .. } | TableFactor::TableFunction { alias: a, .. } => *a = alias,
            _ => {}
        };
        let replacement = match name.as_str() {
            "FLATTEN" => {
                let input = named("INPUT", Some(0)).ok_or_else(|| SqlError::Arguments("FLATTEN".into()))?;
                let recursive = named("RECURSIVE", Some(3))
                    .is_some_and(|e| matches!(&e, Expr::Value(v) if v.value == Value::Boolean(true)));
                let path = named("PATH", Some(1))
                    .as_ref()
                    .and_then(string_literal)
                    .map(str::to_owned)
                    .filter(|p| !p.is_empty());
                let outer = named("OUTER", Some(2))
                    .is_some_and(|e| matches!(&e, Expr::Value(v) if v.value == Value::Boolean(true)));
                let mode = named("MODE", Some(4)).as_ref().and_then(string_literal).unwrap_or("BOTH").to_uppercase();
                variant::flatten(variant::FlattenArgs { input, path, outer, recursive, mode }, alias)?
            }
            "GENERATOR" => {
                let rowcount = named("ROWCOUNT", Some(0)).ok_or_else(|| SqlError::Arguments("GENERATOR".into()))?;
                variant::generator(rowcount, alias)?
            }
            "SPLIT_TO_TABLE" => {
                let input = named("", Some(0)).ok_or_else(|| SqlError::Arguments("SPLIT_TO_TABLE".into()))?;
                let delimiter = named("", Some(1)).ok_or_else(|| SqlError::Arguments("SPLIT_TO_TABLE".into()))?;
                variant::split_to_table(input, delimiter, alias)?
            }
            "STRTOK_SPLIT_TO_TABLE" => {
                let input = named("", Some(0)).ok_or_else(|| SqlError::Arguments("STRTOK_SPLIT_TO_TABLE".into()))?;
                let delimiters = named("", Some(1)).unwrap_or_else(|| string(" "));
                variant::strtok_split_to_table(input, delimiters, alias)?
            }
            "RESULT_SCAN" => {
                let id =
                    named("", Some(0)).as_ref().and_then(string_literal).map(str::to_owned).ok_or_else(|| {
                        SqlError::Unsupported("RESULT_SCAN needs a query ID or LAST_QUERY_ID()".into())
                    })?;
                let table = result_table(&id);
                self.result_scans.push(id);
                TableFactor::Table {
                    name: ObjectName(vec![
                        ObjectNamePart::Identifier(ident("temp")),
                        ObjectNamePart::Identifier(ident("main")),
                        ObjectNamePart::Identifier(quoted_ident(&table)),
                    ]),
                    alias,
                    args: None,
                    with_hints: vec![],
                    version: None,
                    with_ordinality: false,
                    partitions: vec![],
                    json_path: None,
                    sample: None,
                    index_hints: vec![],
                }
            }
            // Other table functions, e.g. SQL UDFs: DuckDB calls table macros without TABLE(...).
            _ => match factor {
                TableFactor::TableFunction { expr: Expr::Function(f), .. } => {
                    TableFactor::Function { lateral: false, name: f.name.clone(), args, alias, with_ordinality: false }
                }
                _ => {
                    restore_alias(factor, alias);
                    return Ok(None);
                }
            },
        };
        Ok(Some(replacement))
    }
}

/// The temp table the engine materializes a stored result into for `RESULT_SCAN`.
pub fn result_table(query_id: &str) -> String {
    format!("__lf_result_{}", query_id.replace('-', "_"))
}

fn all_patterns(pattern: &Expr) -> Option<Vec<Expr>> {
    let Expr::Function(f) = pattern else { return None };
    if !f.name.to_string().eq_ignore_ascii_case("ALL") {
        return None;
    }
    let FunctionArguments::List(list) = &f.args else { return None };
    list.args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.clone()),
            _ => None,
        })
        .collect()
}

fn like_any(
    subject: Expr,
    pattern: Expr,
    negated: bool,
    ilike: bool,
    escape_char: Option<Box<Expr>>,
    all: bool,
) -> Expr {
    let patterns = match pattern {
        Expr::Tuple(items) => items,
        Expr::Nested(inner) => vec![*inner],
        other => vec![other],
    };
    let matches = patterns
        .into_iter()
        .map(|p| {
            let (expr, pattern, escape_char) = (Box::new(nested(subject.clone())), Box::new(p), escape_char.clone());
            if ilike {
                Expr::ILike { negated: false, any: false, expr, pattern, escape_char }
            } else {
                Expr::Like { negated: false, any: false, expr, pattern, escape_char }
            }
        })
        .collect();
    let any = Expr::Nested(Box::new(if all { and_all(matches) } else { or_all(matches) }));
    if negated { Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Not, expr: Box::new(any) } } else { any }
}

fn minus_to_except(body: &mut SetExpr) {
    if let SetExpr::SetOperation { op, left, right, .. } = body {
        if *op == SetOperator::Minus {
            *op = SetOperator::Except;
        }
        minus_to_except(left);
        minus_to_except(right);
    }
}

/// Whether an interval literal only has year, month, week or day parts.
fn date_only_interval(interval: &sqlparser::ast::Interval) -> bool {
    use sqlparser::ast::DateTimeField as F;
    match &interval.leading_field {
        Some(F::Year | F::Years | F::Month | F::Months | F::Week(_) | F::Weeks | F::Day | F::Days) => true,
        Some(_) => false,
        None => string_literal(&interval.value).is_some_and(|text| {
            text.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphabetic())).all(|unit| {
                Part::parse(unit.trim_end_matches(','))
                    .is_some_and(|p| matches!(p, Part::Year | Part::Quarter | Part::Month | Part::Week | Part::Day))
            })
        }),
    }
}

/// A string literal holding a number, as a number literal.
fn numeric_literal(expr: &Expr) -> Option<Expr> {
    let text = string_literal(expr)?.trim();
    text.parse::<f64>().ok().filter(|f| f.is_finite())?;
    Some(Expr::Value(Value::Number(text.to_owned(), false).into()))
}

fn is_niladic(name: &str) -> bool {
    matches!(
        name,
        "CURRENT_USER" | "CURRENT_ROLE" | "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP" | "CURRENT_TIME" | "LOCALTIME"
    )
}

fn parse_object_name(name: &str) -> ObjectName {
    ObjectName(
        name.split('.')
            .map(|part| {
                let ident = match part.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                    Some(quoted) => quoted_ident(&crate::encode_identifier(quoted)),
                    None => Ident::new(part.to_uppercase()),
                };
                ObjectNamePart::Identifier(ident)
            })
            .collect(),
    )
}

fn table_alias(name: &str) -> sqlparser::ast::TableAlias {
    sqlparser::ast::TableAlias { name: quoted_ident(name), columns: vec![], explicit: true, at: None }
}

fn take(expr: &mut Box<Expr>) -> Expr {
    std::mem::replace(expr.as_mut(), null())
}
