//! Translation of Snowflake SQL into commands the engine runs against DuckDB.
//!
//! The pipeline for each request:
//! 1. [`split`] normalizes identifier case and splits the text into statements.
//! 2. [`translate_statement`] turns one statement into a [`Command`]: session
//!    commands (`USE`, `ALTER SESSION`, ...) for the engine to apply, or DuckDB
//!    SQL produced by parsing with sqlparser and rewriting the AST.
//!
//! [`MACROS`] holds the SQL helper macros the rewritten SQL relies on; the
//! engine creates them in every DuckDB connection.

mod ast;
mod connect_by;
mod context;
mod datetime;
mod functions;
mod information_schema;
mod match_recognize;
mod meta;
mod normalize;
mod rewrite;
pub mod stage;
mod translate;
mod types;
mod typing;
mod udf;
mod variant;

pub use context::{Context, DEFAULT_PARAMETERS, TimestampMapping};
pub use meta::{Scope, Show};
pub use normalize::{CASE_MARKER, decode_identifier, encode_identifier, normalize_identifiers};
pub use rewrite::result_table;
pub use translate::{Command, InsertParts, SqlCommand, StatementKind, UseTarget, split, translate_statement};
pub use typing::SiteTypes;

/// SQL macros the translated SQL depends on.
pub fn macros() -> String {
    let sequences: String = (0..typing::SEQUENCES)
        .map(|i| format!("CREATE OR REPLACE TEMP SEQUENCE __lf_seq{i} MINVALUE 0 START 0;\n"))
        .collect();
    format!("{}\n{}\n{}\n{sequences}", functions::MACROS, typing::MACROS, information_schema::MACROS)
}

#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error("{0}")]
    Tokenize(#[from] sqlparser::tokenizer::TokenizerError),
    #[error("{0}")]
    Parse(#[from] sqlparser::parser::ParserError),
    #[error("Empty SQL statement.")]
    Empty,
    #[error("Unsupported feature '{0}'.")]
    Unsupported(String),
    #[error("Invalid number of arguments for function {0}.")]
    Arguments(String),
    #[error(
        "Cannot perform operation. This session does not have a current database. Call 'USE DATABASE', or use a qualified name."
    )]
    NoDatabase,
    #[error("Session variable '${0}' does not exist")]
    UnknownVariable(String),
}
