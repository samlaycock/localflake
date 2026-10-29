//! Runs raw DuckDB SQL with the same build localflake uses, for checking
//! DuckDB behaviour while writing rewrites:
//!
//!     cargo run -q -p localflake-engine --example duckdb -- "select 1" "select typeof(1)"

use duckdb::Connection;
use duckdb::arrow::util::pretty::pretty_format_batches;

fn main() {
    let conn = Connection::open_in_memory().unwrap();
    for sql in std::env::args().skip(1) {
        println!("> {sql}");
        let result = conn.prepare(&sql).and_then(|mut stmt| Ok(stmt.query_arrow([])?.collect::<Vec<_>>()));
        match result {
            Ok(batches) => match pretty_format_batches(&batches) {
                Ok(table) => println!("{table}"),
                Err(err) => println!("(cannot display: {err})\n{batches:?}"),
            },
            Err(err) => println!("ERROR: {err}"),
        }
    }
}
