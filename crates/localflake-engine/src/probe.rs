//! The type probe behind type-directed rewrites.
//!
//! Some Snowflake results depend on argument types the translator can't see,
//! e.g. the scale of `a / b` follows the scale of `a`. In probe mode the
//! translator replaces each such site with `sf_probe(site_id, operand, ...)`.
//! Preparing that SQL makes DuckDB bind every site; `sf_probe`'s bind callback
//! records the operand types, and the translator then runs again with them.
//!
//! When one probe's argument contains another, DuckDB hands the outer bind
//! callback the inner call's arguments, so only innermost sites are recorded
//! per prepare. The engine therefore probes in rounds (see `Session::translate`).
//!
//! duckdb-rs doesn't expose scalar bind callbacks, so `sf_probe` is registered
//! through the C API. Binding happens on the thread that prepares the
//! statement, so the recorded types live in a thread-local.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::ptr;

use duckdb::ffi;

thread_local! {
    static PROBED: RefCell<HashMap<u32, Vec<String>>> = RefCell::new(HashMap::new());
}

/// Prepares `sql` (without running it) and returns the operand types of every
/// `sf_probe` site DuckDB bound. Sites behind a bind error are missing.
pub fn site_types(conn: &duckdb::Connection, sql: &str) -> HashMap<u32, Vec<String>> {
    PROBED.with(|p| p.borrow_mut().clear());
    if let Err(err) = conn.prepare(sql) {
        tracing::debug!(%err, "type probe did not bind");
    }
    PROBED.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Registers the probes on `db`: `sf_probe(INTEGER, ANY[, ANY]) -> DOUBLE` for
/// numeric sites and `sf_probe_text(INTEGER, ANY) -> VARCHAR` for text sites,
/// so the probe SQL still binds where the site's value is used.
///
/// # Safety
/// `db` must be an open database.
pub unsafe fn register(db: ffi::duckdb_database) -> Result<(), String> {
    unsafe {
        register_probe(db, c"sf_probe", ffi::DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE)?;
        register_probe(db, c"sf_probe_text", ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR)
    }
}

unsafe fn register_probe(db: ffi::duckdb_database, name: &CStr, return_type: ffi::duckdb_type) -> Result<(), String> {
    unsafe {
        let mut con: ffi::duckdb_connection = ptr::null_mut();
        if ffi::duckdb_connect(db, &mut con) != ffi::duckdb_state_DuckDBSuccess {
            return Err("cannot connect to register the type probes".into());
        }
        let mut set = ffi::duckdb_create_scalar_function_set(name.as_ptr());
        for arity in 1..=2 {
            let mut f = ffi::duckdb_create_scalar_function();
            ffi::duckdb_scalar_function_set_name(f, name.as_ptr());
            add_parameter(f, ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER);
            for _ in 0..arity {
                add_parameter(f, ffi::DUCKDB_TYPE_DUCKDB_TYPE_ANY);
            }
            let mut returns = ffi::duckdb_create_logical_type(return_type);
            ffi::duckdb_scalar_function_set_return_type(f, returns);
            ffi::duckdb_destroy_logical_type(&mut returns);
            ffi::duckdb_scalar_function_set_function(f, Some(execute));
            ffi::duckdb_scalar_function_set_bind(f, Some(bind));
            ffi::duckdb_add_scalar_function_to_set(set, f);
            ffi::duckdb_destroy_scalar_function(&mut f);
        }
        let state = ffi::duckdb_register_scalar_function_set(con, set);
        ffi::duckdb_destroy_scalar_function_set(&mut set);
        ffi::duckdb_disconnect(&mut con);
        if state == ffi::duckdb_state_DuckDBSuccess {
            Ok(())
        } else {
            Err(format!("cannot register {}", name.to_string_lossy()))
        }
    }
}

unsafe fn add_parameter(f: ffi::duckdb_scalar_function, type_id: ffi::duckdb_type) {
    unsafe {
        let mut t = ffi::duckdb_create_logical_type(type_id);
        ffi::duckdb_scalar_function_add_parameter(f, t);
        ffi::duckdb_destroy_logical_type(&mut t);
    }
}

/// Probe SQL is only prepared, but DuckDB may still constant-fold it; return NULLs.
unsafe extern "C" fn execute(_: ffi::duckdb_function_info, input: ffi::duckdb_data_chunk, output: ffi::duckdb_vector) {
    unsafe {
        let rows = ffi::duckdb_data_chunk_get_size(input);
        ffi::duckdb_vector_ensure_validity_writable(output);
        let validity = ffi::duckdb_vector_get_validity(output);
        for row in 0..rows {
            ffi::duckdb_validity_set_row_invalid(validity, row);
        }
    }
}

unsafe extern "C" fn bind(info: ffi::duckdb_bind_info) {
    unsafe {
        let count = ffi::duckdb_scalar_function_bind_get_argument_count(info);
        let mut context: ffi::duckdb_client_context = ptr::null_mut();
        ffi::duckdb_scalar_function_get_client_context(info, &mut context);
        let mut id_expr = ffi::duckdb_scalar_function_bind_get_argument(info, 0);
        let mut value: ffi::duckdb_value = ptr::null_mut();
        let mut error = ffi::duckdb_expression_fold(context, id_expr, &mut value);
        ffi::duckdb_destroy_expression(&mut id_expr);
        ffi::duckdb_destroy_client_context(&mut context);
        if !error.is_null() {
            ffi::duckdb_destroy_error_data(&mut error);
            return;
        }
        let id = ffi::duckdb_get_int32(value) as u32;
        ffi::duckdb_destroy_value(&mut value);
        let types = (1..count)
            .map(|i| {
                let mut expr = ffi::duckdb_scalar_function_bind_get_argument(info, i);
                let mut logical = ffi::duckdb_expression_return_type(expr);
                let name = type_name(logical);
                ffi::duckdb_destroy_logical_type(&mut logical);
                ffi::duckdb_destroy_expression(&mut expr);
                name
            })
            .collect();
        PROBED.with(|p| p.borrow_mut().insert(id, types));
    }
}

unsafe fn type_name(logical: ffi::duckdb_logical_type) -> String {
    let id = unsafe { ffi::duckdb_get_type_id(logical) };
    let name = match id {
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL => {
            let (width, scale) = unsafe { (ffi::duckdb_decimal_width(logical), ffi::duckdb_decimal_scale(logical)) };
            return format!("DECIMAL({width},{scale})");
        }
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN => "BOOLEAN",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TINYINT => "TINYINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT => "SMALLINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER | ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER_LITERAL => "INTEGER",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT => "BIGINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT => "HUGEINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT => "UTINYINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT => "USMALLINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER => "UINTEGER",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT => "UBIGINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT => "UHUGEINT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_FLOAT => "FLOAT",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE => "DOUBLE",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR | ffi::DUCKDB_TYPE_DUCKDB_TYPE_STRING_LITERAL => "VARCHAR",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_SQLNULL => "NULL",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_DATE => "DATE",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TIME => "TIME",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP => "TIMESTAMP",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ => "TIMESTAMPTZ",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS => "TIMESTAMP_NS",
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARIANT => "VARIANT",
        _ => "OTHER",
    };
    name.to_owned()
}
