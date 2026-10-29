//! Dumps sqlparser's AST and re-rendered SQL for Snowflake statements:
//!
//!     cargo run -q -p localflake-sql --example parse -- "select v:a from t"

use sqlparser::dialect::SnowflakeDialect;
use sqlparser::parser::Parser;

fn main() {
    let debug = std::env::var("AST").is_ok();
    for sql in std::env::args().skip(1) {
        match Parser::parse_sql(&SnowflakeDialect, &sql) {
            Ok(statements) => {
                for s in statements {
                    println!("{sql}\n  => {s}");
                    if debug {
                        println!("{s:#?}");
                    }
                }
            }
            Err(e) => println!("{sql}\n  !! {e}"),
        }
    }
}
