//! Persistent secondary connection for executing SQL from extension callbacks.
//!
//! DuckDB forbids querying the main connection from within scalar/table
//! function callbacks. We keep ONE persistent connection created in the
//! init callback (early connect — required on macOS ARM64) and reuse it
//! for every assertion query. Never disconnect.

use libduckdb_sys::*;
use std::error::Error;
use std::ffi::CStr;
use std::ffi::CString;
use std::mem;
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

use quack_rs::connection::Connection;

/// duckdb_database is `*mut _duckdb_database` — not Sync.
/// Store as AtomicPtr<c_void> with acquire/release ordering.
pub static DB_STORE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Wrapper to make duckdb_connection Send + Sync.
/// SAFETY: the connection is used exclusively through a Mutex.
pub struct ConnHandle(pub duckdb_connection);
unsafe impl Send for ConnHandle {}
unsafe impl Sync for ConnHandle {}

static CONN: Mutex<Option<ConnHandle>> = Mutex::new(None);

/// Connect immediately in the init callback (critical on macOS ARM64 where
/// the database handle becomes invalid after the init closure returns).
pub fn init_early(con: &Connection) {
    let db = con.as_raw_database();
    DB_STORE.store(db as *mut c_void, Ordering::Release);
    unsafe {
        let mut early_con: duckdb_connection = ptr::null_mut();
        if duckdb_connect(db, &mut early_con) == DuckDBSuccess {
            let mut guard = CONN.lock().unwrap();
            *guard = Some(ConnHandle(early_con));
        }
    }
}

/// Run a query with the persistent connection; closure receives `&mut duckdb_connection`.
fn with_conn<T>(
    f: impl FnOnce(&mut duckdb_connection) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let mut guard = CONN.lock().unwrap();
    unsafe {
        if guard.is_none() {
            let db = DB_STORE.load(Ordering::Acquire) as duckdb_database;
            if db.is_null() {
                return Err("db handle not set — extension init incomplete".into());
            }
            let mut con: duckdb_connection = ptr::null_mut();
            if duckdb_connect(db, &mut con) != DuckDBSuccess {
                return Err("duckdb_connect failed".into());
            }
            *guard = Some(ConnHandle(con));
        }
        let con = &mut guard.as_mut().unwrap().0;
        f(con)
    }
}

/// Run a query and return all rows as Vec<Vec<String>> (NULL → empty string).
pub fn run_query_rows(sql: &str) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
    with_conn(|con| unsafe {
        let sql_c = CString::new(sql.as_bytes())?;
        let mut result: duckdb_result = mem::zeroed();
        let rc = duckdb_query(*con, sql_c.as_ptr(), &mut result);

        if rc != DuckDBSuccess {
            let err = CStr::from_ptr(duckdb_result_error(&mut result))
                .to_string_lossy()
                .into_owned();
            duckdb_destroy_result(&mut result);
            return Err(err.into());
        }

        let ncols = duckdb_column_count(&mut result) as usize;
        let nrows = duckdb_row_count(&mut result) as usize;

        let mut rows: Vec<Vec<String>> = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut row = Vec::with_capacity(ncols);
            for c in 0..ncols {
                if duckdb_value_is_null(&mut result, c as u64, r as u64) {
                    row.push(String::new());
                } else {
                    let p = duckdb_value_varchar(&mut result, c as u64, r as u64);
                    if p.is_null() {
                        row.push(String::new());
                    } else {
                        row.push(CStr::from_ptr(p).to_string_lossy().into_owned());
                    }
                }
            }
            rows.push(row);
        }

        duckdb_destroy_result(&mut result);
        Ok(rows)
    })
}

/// Run a statement (DDL/DML, no result set) — multi-statement supported.
pub fn run_exec(sql: &str) -> Result<(), Box<dyn Error>> {
    with_conn(|con| unsafe {
        let sql_c = CString::new(sql.as_bytes())?;
        let mut result: duckdb_result = mem::zeroed();
        let rc = duckdb_query(*con, sql_c.as_ptr(), &mut result);
        if rc != DuckDBSuccess {
            let err = CStr::from_ptr(duckdb_result_error(&mut result))
                .to_string_lossy()
                .into_owned();
            duckdb_destroy_result(&mut result);
            return Err(err.into());
        }
        duckdb_destroy_result(&mut result);
        Ok(())
    })
}

// Re-export duckdb_sys members used by lib.rs
pub use libduckdb_sys::{duckdb_connection, duckdb_database};
