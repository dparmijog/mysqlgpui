//! Postgres backend: implements [`Driver`] on top of `sqlx::PgPool`.
//!
//! Unlike MySQL, a Postgres connection is permanently scoped to the one
//! database it connected to — there is no server-wide "see every
//! database" connection, and no cross-database `db.table` syntax. That
//! shapes two deliberate differences from `mysql.rs`:
//!
//! - [`PgDriver::schema`] returns only the connected database (not every
//!   database on the server) — [`PgDriver::list_databases`] is the one
//!   that lists everything, and it's only ever used by the wizard
//!   *before* a specific database has been picked.
//! - [`PgDriver::tables`] errors if asked for any database other than the
//!   one it's connected to, instead of silently returning nothing.

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Column as _, Row as _, TypeInfo as _};
use tokio_util::sync::CancellationToken;

use crate::driver::{Column, ConnConfig, Dialect, Driver, DriverError, ForeignKey, Plan, Row, Schema, Table, Value};

pub struct PgDialect;

impl Dialect for PgDialect {
    fn quote_ident(&self, s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    fn limit(&self, sql: &str, n: u64) -> String {
        let trimmed = sql.trim_end().trim_end_matches(';');
        format!("{trimmed} LIMIT {n}")
    }

    fn keywords(&self) -> &'static [&'static str] {
        PG_KEYWORDS
    }

    fn qualify_table(&self, _db: &str, table: &str) -> String {
        // Already connected to exactly one database — `db` would be
        // misread as a schema name, so just quote the table.
        self.quote_ident(table)
    }
}

/// Postgres's reserved-word vocabulary for the editor's syntax
/// highlighting and autocomplete — not exhaustive, but covers everyday
/// DML/DDL/DQL vocabulary.
const PG_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "AS", "DISTINCT",
    "JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "CROSS", "ON", "USING", "UNION", "ALL", "EXISTS",
    "AND", "OR", "NOT", "NULL", "IS", "IN", "LIKE", "ILIKE", "BETWEEN", "CASE", "WHEN", "THEN", "ELSE", "END",
    "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "RETURNING",
    "CREATE", "ALTER", "DROP", "TABLE", "DATABASE", "SCHEMA", "INDEX", "VIEW", "TRIGGER", "FUNCTION",
    "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "CONSTRAINT", "UNIQUE", "DEFAULT", "SERIAL", "BIGSERIAL",
    "CASCADE", "RESTRICT", "CHECK", "NOT NULL",
    "INT", "INTEGER", "BIGINT", "SMALLINT", "NUMERIC", "REAL", "DOUBLE PRECISION", "BOOLEAN",
    "CHAR", "VARCHAR", "TEXT", "BYTEA", "DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "JSON", "JSONB", "UUID", "ARRAY",
    "EXPLAIN", "ANALYZE", "DESC", "ASC", "WITH", "BEGIN", "COMMIT", "ROLLBACK", "TRANSACTION",
    "GRANT", "REVOKE",
];

pub struct PgDriver {
    pool: PgPool,
    /// The one database this connection is scoped to — Postgres has no
    /// server-wide connection, so every `PgDriver` belongs to exactly one.
    database: String,
    dialect: PgDialect,
}

/// Decodes a single Postgres cell into our backend-agnostic [`Value`],
/// based on the column's reported type name. Unknown types fall back to
/// their textual form rather than failing the whole row.
fn decode_value(row: &PgRow, idx: usize, type_name: &str) -> anyhow::Result<Value> {
    macro_rules! try_as {
        ($t:ty, $wrap:expr) => {
            row.try_get::<Option<$t>, _>(idx)
                .map(|v| v.map($wrap).unwrap_or(Value::Null))
        };
    }

    let value = match type_name {
        "INT2" => row.try_get::<Option<i16>, _>(idx).map(|v| v.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
        "INT4" => row.try_get::<Option<i32>, _>(idx).map(|v| v.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
        "INT8" => try_as!(i64, Value::Int),
        "FLOAT4" => row.try_get::<Option<f32>, _>(idx).map(|v| v.map(|n| Value::Float(n as f64)).unwrap_or(Value::Null)),
        "FLOAT8" => try_as!(f64, Value::Float),
        "NUMERIC" => row
            .try_get::<Option<rust_decimal::Decimal>, _>(idx)
            .map(|v| v.map(|d| Value::Text(d.to_string())).unwrap_or(Value::Null)),
        "BOOL" => try_as!(bool, Value::Bool),
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => try_as!(String, Value::Text),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)
            .map(|v| v.map(|d| Value::Text(d.to_string())).unwrap_or(Value::Null)),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)
            .map(|v| v.map(|t| Value::Text(t.to_string())).unwrap_or(Value::Null)),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
            .map(|v| v.map(|t| Value::Text(t.to_string())).unwrap_or(Value::Null)),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)
            .map(|v| v.map(|t| Value::Text(t.to_string())).unwrap_or(Value::Null)),
        "BYTEA" => row.try_get::<Option<Vec<u8>>, _>(idx).map(|v| v.map(Value::Bytes).unwrap_or(Value::Null)),
        "JSON" | "JSONB" => row
            .try_get::<Option<serde_json::Value>, _>(idx)
            .map(|v| v.map(|j| Value::Text(j.to_string())).unwrap_or(Value::Null)),
        "UUID" => row
            .try_get::<Option<sqlx::types::Uuid>, _>(idx)
            .map(|v| v.map(|u| Value::Text(u.to_string())).unwrap_or(Value::Null)),
        other => {
            // Best-effort fallback: try a string decode; if that fails
            // too, surface the type name so at least the shape is visible.
            match row.try_get::<Option<String>, _>(idx) {
                Ok(v) => Ok(v.map(Value::Text).unwrap_or(Value::Null)),
                Err(_) => Ok(Value::Other(format!("<{other}>"))),
            }
        }
    };

    value.map_err(|e| anyhow::anyhow!("decoding column {idx} ({type_name}): {e}"))
}

fn row_from_pg(row: PgRow) -> anyhow::Result<Row> {
    let mut cols = Vec::with_capacity(row.columns().len());
    let mut values = Vec::with_capacity(row.columns().len());
    for (idx, col) in row.columns().iter().enumerate() {
        cols.push(col.name().to_string());
        values.push(decode_value(&row, idx, col.type_info().name())?);
    }
    Ok(Row { cols, values })
}

#[async_trait]
impl Driver for PgDriver {
    async fn connect(cfg: &ConnConfig) -> Result<Self, DriverError> {
        let pool = PgPool::connect(&cfg.url).await.map_err(classify_connect_error)?;
        let database = pool.connect_options().get_database().unwrap_or("postgres").to_string();
        Ok(PgDriver { pool, database, dialect: PgDialect })
    }

    fn query<'a>(&'a self, sql: &'a str, cancel: CancellationToken) -> BoxStream<'a, anyhow::Result<Row>> {
        let rows = sqlx::query(sql)
            .fetch(&self.pool)
            .map(|res| res.map_err(anyhow::Error::from).and_then(row_from_pg));
        rows.take_until(cancel.cancelled_owned()).boxed()
    }

    async fn execute(&self, sql: &str) -> anyhow::Result<u64> {
        let result = sqlx::query(sql).execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    async fn schema(&self) -> anyhow::Result<Schema> {
        // A Postgres connection can never see another database's catalog
        // — unlike MySQL's server-wide connection, so there's nothing
        // else to list here. `list_databases` (used by the wizard, before
        // any database has been picked) is what shows the full server.
        Ok(Schema { databases: vec![self.database.clone()] })
    }

    async fn tables(&self, db_name: &str) -> anyhow::Result<Vec<Table>> {
        if db_name != self.database {
            anyhow::bail!(
                "cannot browse database '{db_name}' from a Postgres connection to '{}': \
                 Postgres has no cross-database queries — reconnect via the wizard with \
                 '{db_name}' selected",
                self.database
            );
        }

        let table_rows = sqlx::query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
        )
        .fetch_all(&self.pool)
        .await?;
        let table_names =
            table_rows.iter().map(|r| r.try_get::<String, _>(0)).collect::<Result<Vec<_>, _>>()?;

        let pk_rows = sqlx::query(
            "SELECT tc.table_name, kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
             WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = 'public'",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut pk_columns: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
        for r in &pk_rows {
            pk_columns.insert((r.try_get::<String, _>(0)?, r.try_get::<String, _>(1)?));
        }

        let col_rows = sqlx::query(
            "SELECT table_name, column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = 'public' ORDER BY table_name, ordinal_position",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut columns_by_table: std::collections::HashMap<String, Vec<Column>> =
            std::collections::HashMap::new();
        for r in &col_rows {
            let table_name = r.try_get::<String, _>(0)?;
            let column_name = r.try_get::<String, _>(1)?;
            let key = pk_columns
                .contains(&(table_name.clone(), column_name.clone()))
                .then(|| "PRI".to_string());
            let column = Column {
                name: column_name,
                ty: r.try_get::<String, _>(2)?,
                nullable: r.try_get::<String, _>(3)?.eq_ignore_ascii_case("YES"),
                key,
            };
            columns_by_table.entry(table_name).or_default().push(column);
        }

        let idx_rows = sqlx::query(
            "SELECT DISTINCT tablename, indexname FROM pg_indexes \
             WHERE schemaname = 'public' ORDER BY tablename, indexname",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut indexes_by_table: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for r in &idx_rows {
            let table_name = r.try_get::<String, _>(0)?;
            indexes_by_table.entry(table_name).or_default().push(r.try_get::<String, _>(1)?);
        }

        // `information_schema.constraint_column_usage` only shows a
        // referenced-table column when the connected user owns (or has a
        // non-SELECT privilege on) *that specific* table — a real-world
        // read/write grant on the referencing table alone isn't enough,
        // so it silently drops FKs for exactly the kind of non-owner
        // account this tool is built to be used with. `pg_constraint`
        // has no such filtering: any role that can see the table at all
        // sees its constraints. `unnest` zips `conkey`/`confkey` pairwise
        // to support composite foreign keys, not just single-column ones.
        let fk_rows = sqlx::query(
            "SELECT con.conrelid::regclass::text, a1.attname, con.confrelid::regclass::text, a2.attname \
             FROM pg_constraint con \
             JOIN LATERAL unnest(con.conkey, con.confkey) AS cols(conkey, confkey) ON true \
             JOIN pg_attribute a1 ON a1.attrelid = con.conrelid AND a1.attnum = cols.conkey \
             JOIN pg_attribute a2 ON a2.attrelid = con.confrelid AND a2.attnum = cols.confkey \
             JOIN pg_namespace ns ON ns.oid = con.connamespace \
             WHERE con.contype = 'f' AND ns.nspname = 'public' \
             ORDER BY 1, 2",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
            std::collections::HashMap::new();
        for r in &fk_rows {
            let table_name = r.try_get::<String, _>(0)?;
            let fk = ForeignKey {
                column: r.try_get::<String, _>(1)?,
                ref_table: r.try_get::<String, _>(2)?,
                ref_column: r.try_get::<String, _>(3)?,
            };
            fks_by_table.entry(table_name).or_default().push(fk);
        }

        let tables = table_names
            .into_iter()
            .map(|name| {
                let columns = columns_by_table.remove(&name).unwrap_or_default();
                let indexes = indexes_by_table.remove(&name).unwrap_or_default();
                let foreign_keys = fks_by_table.remove(&name).unwrap_or_default();
                Table { name, columns, indexes, foreign_keys }
            })
            .collect();

        Ok(tables)
    }

    async fn list_databases(&self) -> anyhow::Result<Vec<String>> {
        list_database_names(&self.pool).await
    }

    async fn explain(&self, sql: &str) -> anyhow::Result<Plan> {
        let explain_sql = format!("EXPLAIN {sql}");
        let raw_rows = sqlx::query(&explain_sql).fetch_all(&self.pool).await?;
        let rows = raw_rows.into_iter().map(row_from_pg).collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Plan { rows })
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }
}

/// Lists every real database on the server (excluding templates) — a
/// shared catalog visible from any connected database, so this works
/// regardless of which database `pool` happens to be connected to.
async fn list_database_names(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query("SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname")
        .fetch_all(pool)
        .await?;
    rows.iter().map(|r| r.try_get::<String, _>(0).map_err(anyhow::Error::from)).collect()
}

/// Maps a `sqlx::Error` from a failed connection attempt to a
/// [`DriverError`]. Database-protocol errors are classified by Postgres's
/// SQLSTATE code via [`classify_postgres_error_code`]; transport-level
/// failures (refused/timed-out/DNS) surface as `sqlx::Error::Io`/
/// `PoolTimedOut` and map to `ConnectionRefused`.
fn classify_connect_error(e: sqlx::Error) -> DriverError {
    match &e {
        sqlx::Error::Database(db_err) => match db_err.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
            Some(pg_err) => classify_postgres_error_code(pg_err.code(), pg_err.message()),
            None => DriverError::Other(anyhow::anyhow!(db_err.message().to_string())),
        },
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => DriverError::ConnectionRefused(e.to_string()),
        _ => DriverError::Other(anyhow::anyhow!(e.to_string())),
    }
}

/// Pure classification by Postgres's SQLSTATE code — kept separate from
/// [`classify_connect_error`] specifically so it's testable without a
/// live server. Reference: <https://www.postgresql.org/docs/current/errcodes-appendix.html>
fn classify_postgres_error_code(code: &str, message: &str) -> DriverError {
    match code {
        // invalid_password, invalid_authorization_specification
        "28P01" | "28000" => DriverError::AuthFailed(message.to_string()),
        // invalid_catalog_name
        "3D000" => DriverError::UnknownDatabase(message.to_string()),
        _ => DriverError::Other(anyhow::anyhow!("{message}")),
    }
}

#[cfg(test)]
mod connect_error_tests {
    use super::*;

    #[test]
    fn classifies_invalid_password_as_auth_failed() {
        assert!(matches!(
            classify_postgres_error_code("28P01", "password authentication failed for user \"x\""),
            DriverError::AuthFailed(_)
        ));
    }

    #[test]
    fn classifies_invalid_authorization_as_auth_failed() {
        assert!(matches!(classify_postgres_error_code("28000", "no pg_hba.conf entry"), DriverError::AuthFailed(_)));
    }

    #[test]
    fn classifies_invalid_catalog_name_as_unknown_database() {
        assert!(matches!(
            classify_postgres_error_code("3D000", "database \"nope\" does not exist"),
            DriverError::UnknownDatabase(_)
        ));
    }

    #[test]
    fn classifies_unrecognized_codes_as_other() {
        assert!(matches!(classify_postgres_error_code("42601", "syntax error"), DriverError::Other(_)));
    }

    #[test]
    fn display_text_is_actionable() {
        let err = classify_postgres_error_code("28P01", "password authentication failed for user \"x\"");
        assert!(err.to_string().contains("authentication failed"));
    }
}
