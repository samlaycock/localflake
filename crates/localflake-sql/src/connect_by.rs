//! `START WITH ... CONNECT BY ...` hierarchical queries as recursive CTEs.
//!
//! ```sql
//! SELECT name, LEVEL, SYS_CONNECT_BY_PATH(name, '/') FROM emp
//! START WITH mgr IS NULL CONNECT BY PRIOR id = mgr
//! ```
//! becomes a `WITH RECURSIVE __CB` whose anchor is the START WITH rows and
//! whose step joins each level's rows to their children. The CTE carries
//! `__LEVEL`, one accumulated path per `SYS_CONNECT_BY_PATH` call and one root
//! value per `CONNECT_BY_ROOT` expression; the original SELECT then reads
//! from it with those calls replaced by the columns.

use std::ops::ControlFlow;

use sqlparser::ast::{
    ConnectByKind, Cte, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, Query, SetExpr,
    TableAlias, TableFactor, VisitMut, VisitorMut, With,
};
use sqlparser::dialect::SnowflakeDialect;
use sqlparser::parser::Parser;

use crate::SqlError;

/// Hierarchies deeper than this are assumed to be cycles (Snowflake errors on cycles).
const MAX_DEPTH: u32 = 10_000;

pub fn rewrite(query: &mut Query) -> Result<(), SqlError> {
    let SetExpr::Select(select) = query.body.as_mut() else { return Ok(()) };
    if select.connect_by.is_empty() {
        return Ok(());
    }
    let mut start_with = None;
    let mut relationships = vec![];
    for kind in std::mem::take(&mut select.connect_by) {
        match kind {
            ConnectByKind::StartWith { condition, .. } => start_with = Some(*condition),
            ConnectByKind::ConnectBy { relationships: r, .. } => relationships = r,
        }
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(SqlError::Unsupported("CONNECT BY over joins".into()));
    }
    let relation = &mut select.from[0].relation;
    let alias = match relation {
        TableFactor::Table { alias, name, .. } => {
            alias.take().map(|a| a.name.value).or_else(|| name.0.last().map(|p| p.to_string()))
        }
        TableFactor::Derived { alias, .. } => alias.take().map(|a| a.name.value),
        _ => None,
    }
    .ok_or_else(|| SqlError::Unsupported("CONNECT BY on this relation".into()))?;
    let source = relation.to_string();

    // Replace LEVEL, SYS_CONNECT_BY_PATH and CONNECT_BY_ROOT in the SELECT with CTE columns.
    let mut collector = Collector::default();
    let _ = select.projection.visit(&mut collector);
    if let Some(selection) = &mut select.selection {
        let _ = selection.visit(&mut collector);
    }
    let _ = query.order_by.visit(&mut collector);

    let qualify = |e: &Expr| qualified(e.clone(), &alias, "__T");
    let mut anchor_cols = vec!["__T.*".to_owned(), "1 AS __LEVEL".to_owned()];
    let mut step_cols = vec!["__T.*".to_owned(), "__CB.__LEVEL + 1".to_owned()];
    for (i, (expr, separator)) in collector.paths.iter().enumerate() {
        let value = format!("CAST({} AS VARCHAR)", qualify(expr));
        anchor_cols.push(format!("{separator} || {value} AS __P{i}"));
        step_cols.push(format!("__CB.__P{i} || {separator} || {value}"));
    }
    for (i, expr) in collector.roots.iter().enumerate() {
        anchor_cols.push(format!("{} AS __R{i}", qualify(expr)));
        step_cols.push(format!("__CB.__R{i}"));
    }
    let start = start_with.map_or_else(|| "TRUE".to_owned(), |c| qualify(&c).to_string());
    let join = relationships
        .iter()
        .map(|r| qualified(prior_to_parent(r.clone()), &alias, "__T").to_string())
        .collect::<Vec<_>>()
        .join(" AND ");
    let cte_sql = format!(
        "SELECT {} FROM {source} AS __T WHERE {start} UNION ALL \
         SELECT {} FROM {source} AS __T JOIN __CB ON {join} WHERE __CB.__LEVEL < {MAX_DEPTH}",
        anchor_cols.join(", "),
        step_cols.join(", ")
    );
    let cte_query = Parser::new(&SnowflakeDialect).try_with_sql(&cte_sql)?.parse_query()?;

    select.from[0].relation = TableFactor::Table {
        name: sqlparser::ast::ObjectName::from(vec![Ident::new("__CB")]),
        alias: Some(TableAlias { explicit: true, name: Ident::new(&alias), columns: vec![], at: None }),
        args: None,
        with_hints: vec![],
        version: None,
        with_ordinality: false,
        partitions: vec![],
        json_path: None,
        sample: None,
        index_hints: vec![],
    };
    let cte = Cte {
        alias: TableAlias { explicit: false, name: Ident::new("__CB"), columns: vec![], at: None },
        query: cte_query,
        from: None,
        materialized: None,
        closing_paren_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
    };
    match &mut query.with {
        Some(with) => {
            with.recursive = true;
            with.cte_tables.push(cte);
        }
        None => {
            query.with = Some(With {
                with_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                recursive: true,
                cte_tables: vec![cte],
            })
        }
    }
    Ok(())
}

#[derive(Default)]
struct Collector {
    /// `SYS_CONNECT_BY_PATH(expr, separator)` calls, with the separator as SQL.
    paths: Vec<(Expr, String)>,
    roots: Vec<Expr>,
}

impl VisitorMut for Collector {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        match expr {
            Expr::Identifier(ident) if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("LEVEL") => {
                *expr = column("__LEVEL");
            }
            Expr::Prefixed { prefix, value } if prefix.value.eq_ignore_ascii_case("CONNECT_BY_ROOT") => {
                let i = self.roots.len();
                self.roots.push((**value).clone());
                *expr = column(&format!("__R{i}"));
            }
            Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("SYS_CONNECT_BY_PATH") => {
                if let Some((value, separator)) = two_args(f) {
                    let i = self.paths.len();
                    self.paths.push((value, separator.to_string()));
                    *expr = column(&format!("__P{i}"));
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn two_args(f: &Function) -> Option<(Expr, Expr)> {
    let FunctionArguments::List(list) = &f.args else { return None };
    let mut args = list.args.iter().filter_map(|a| match a {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.clone()),
        _ => None,
    });
    Some((args.next()?, args.next()?))
}

fn column(name: &str) -> Expr {
    Expr::Identifier(Ident::new(name))
}

/// `PRIOR x` refers to the parent row, which in the recursive step is `__CB`.
fn prior_to_parent(mut expr: Expr) -> Expr {
    struct Prior;
    impl VisitorMut for Prior {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Prior(inner) = expr {
                let name = match inner.as_ref() {
                    Expr::Identifier(i) => Some(i.clone()),
                    Expr::CompoundIdentifier(parts) => parts.last().cloned(),
                    _ => None,
                };
                if let Some(name) = name {
                    *expr = Expr::CompoundIdentifier(vec![Ident::new("__CB"), name]);
                }
            }
            ControlFlow::Continue(())
        }
    }
    let _ = expr.visit(&mut Prior);
    expr
}

/// Qualifies bare columns, and columns qualified with `alias`, with `table`.
fn qualified(mut expr: Expr, alias: &str, table: &str) -> Expr {
    struct Qualify<'a>(&'a str, &'a str);
    impl VisitorMut for Qualify<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            match expr {
                Expr::Identifier(i) if !i.value.starts_with("__") => {
                    *expr = Expr::CompoundIdentifier(vec![Ident::new(self.1), i.clone()]);
                }
                Expr::CompoundIdentifier(parts) if parts.len() == 2 && parts[0].value.eq_ignore_ascii_case(self.0) => {
                    parts[0] = Ident::new(self.1);
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }
    let _ = expr.visit(&mut Qualify(alias, table));
    expr
}
