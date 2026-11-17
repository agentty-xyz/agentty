//! Schema guards that keep foreign keys indexed and checked queries off full
//! table scans as migrations and queries change.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;
use sqlx::SqlitePool;

use crate::connection::Database;

/// Full-table scans that specific checked queries may perform.
const ALLOWED_SCANS: [AllowedScan; 4] = [
    AllowedScan {
        query: "DELETE FROM session_activity",
        reason: "clearing activity history deletes every row",
        table: "session_activity",
    },
    AllowedScan {
        query: "DROP TABLE session",
        reason: "connection tests drop the table to simulate a damaged schema",
        table: "session",
    },
    AllowedScan {
        query: "SELECT created_at FROM session_activity ORDER BY id",
        reason: "loads the complete activity history for the projects heatmap",
        table: "session_activity",
    },
    AllowedScan {
        query: "FROM project_setting WHERE name = ?",
        reason: "reads one setting for every project; rows are bounded by project count",
        table: "project_setting",
    },
];

/// One accepted full-table scan, matched by plan table and query text.
struct AllowedScan {
    /// Whitespace-normalized substring of the checked query.
    query: &'static str,
    /// Why the scan is acceptable.
    reason: &'static str,
    /// Table or alias named in the `SCAN` plan step.
    table: &'static str,
}

/// Verifies every foreign key has an index that serves parent deletes and
/// child lookups, so cascades and `SET NULL` updates never scan the child
/// table.
#[tokio::test]
async fn every_foreign_key_is_served_by_an_index() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let foreign_keys = foreign_keys(database.pool()).await;
    let mut unindexed = Vec::new();
    for ((table, _), columns) in &foreign_keys {
        let lookups = lookup_keys(database.pool(), table).await;
        if !lookups.iter().any(|lookup| serves(lookup, columns)) {
            unindexed.push(format!("{table} ({})", columns.join(", ")));
        }
    }

    // Assert
    assert!(
        !foreign_keys.is_empty(),
        "schema should declare foreign keys"
    );
    assert!(
        unindexed.is_empty(),
        "Foreign keys without a leading index: {}. Add an index on these columns in a new \
         migration under `crates/ag-store/migrations/`.",
        unindexed.join("; ")
    );
}

/// Verifies no checked query scans a whole table unless `ALLOWED_SCANS`
/// records why that scan is acceptable.
#[tokio::test]
async fn checked_queries_do_not_scan_whole_tables() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let queries = checked_queries();

    // Act
    let mut violations = Vec::new();
    let mut used_allowances = BTreeSet::new();
    for query in &queries {
        let scans = match full_scans(database.pool(), query).await {
            Ok(scans) => scans,
            Err(error) => {
                violations.push(format!("plan failed for `{query}`: {error}"));

                continue;
            }
        };
        for table in scans {
            match ALLOWED_SCANS
                .iter()
                .position(|allowed| allowed.table == table && query.contains(allowed.query))
            {
                Some(index) => {
                    used_allowances.insert(index);
                }
                None => violations.push(format!("SCAN {table} in `{query}`")),
            }
        }
    }
    let unused_allowances = ALLOWED_SCANS
        .iter()
        .enumerate()
        .filter(|(index, _)| !used_allowances.contains(index))
        .map(|(_, allowed)| format!("{} ({})", allowed.table, allowed.reason))
        .collect::<Vec<_>>();

    // Assert
    assert!(
        !queries.is_empty(),
        "`.sqlx` should contain checked queries"
    );
    assert!(
        violations.is_empty(),
        "Checked queries scan whole tables:\n{}\nAdd an index in a new migration under \
         `crates/ag-store/migrations/`, or add an `ALLOWED_SCANS` entry with a reason.",
        violations.join("\n")
    );
    assert!(
        unused_allowances.is_empty(),
        "Remove `ALLOWED_SCANS` entries that no query needs: {}",
        unused_allowances.join("; ")
    );
}

/// Verifies the plan reader treats only bare table scans as full scans.
#[tokio::test]
async fn full_scan_detection_ignores_indexed_and_derived_scans() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let bare_scan = full_scans(database.pool(), "SELECT id FROM agent_run WHERE folder = ?")
        .await
        .expect("bare scan plan should load");
    let indexed_scan = full_scans(
        database.pool(),
        "SELECT session_id FROM agent_run ORDER BY session_id",
    )
    .await
    .expect("indexed scan plan should load");
    let derived_scan = full_scans(
        database.pool(),
        "SELECT value FROM (SELECT 1 AS value UNION SELECT 2) AS derived ORDER BY value",
    )
    .await
    .expect("derived scan plan should load");
    let invalid_plan = full_scans(database.pool(), "SELECT id FROM missing_table").await;

    // Assert
    assert_eq!(bare_scan, ["agent_run"]);
    assert!(indexed_scan.is_empty(), "{indexed_scan:?}");
    assert!(derived_scan.is_empty(), "{derived_scan:?}");
    assert!(invalid_plan.is_err());
}

/// Returns each foreign key's child columns in declaration order, keyed by
/// table and SQLite foreign-key id so composite keys count once.
async fn foreign_keys(pool: &SqlitePool) -> BTreeMap<(String, i64), Vec<String>> {
    let rows = sqlx::query_as::<_, (String, i64, String)>(
        r#"
SELECT m.name, f.id, f."from"
FROM sqlite_master AS m, pragma_foreign_key_list(m.name) AS f
WHERE m.type = 'table'
ORDER BY m.name, f.id, f.seq
"#,
    )
    .fetch_all(pool)
    .await
    .expect("foreign keys should load");
    let mut foreign_keys: BTreeMap<(String, i64), Vec<String>> = BTreeMap::new();
    for (table, id, column) in rows {
        foreign_keys.entry((table, id)).or_default().push(column);
    }

    foreign_keys
}

/// Returns the ordered column lists of `table`'s full indexes and primary key.
async fn lookup_keys(pool: &SqlitePool, table: &str) -> Vec<Vec<String>> {
    let index_columns = sqlx::query_as::<_, (String, Option<String>)>(
        r"
SELECT il.name, ii.name
FROM pragma_index_list(?1) AS il, pragma_index_info(il.name) AS ii
WHERE il.partial = 0
ORDER BY il.name, ii.seqno
",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("index columns should load");
    let primary_key = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("primary key should load");
    let mut indexes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (index, column) in index_columns {
        // Expression columns have no name and end the usable prefix.
        indexes
            .entry(index)
            .or_default()
            .push(column.unwrap_or_default());
    }

    indexes.into_values().chain([primary_key]).collect()
}

/// Returns whether a lookup key's leading columns equal the foreign-key
/// columns in any order, which lets SQLite seek by every key column.
fn serves(lookup: &[String], columns: &[String]) -> bool {
    lookup.len() >= columns.len()
        && lookup[..columns.len()].iter().collect::<BTreeSet<_>>()
            == columns.iter().collect::<BTreeSet<_>>()
}

/// Returns every checked query recorded in `.sqlx`, with whitespace
/// collapsed so allowlist substrings stay stable across formatting.
fn checked_queries() -> Vec<String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(".sqlx");
    let mut paths = fs::read_dir(&directory)
        .expect("`.sqlx` should be readable")
        .map(|entry| entry.expect("`.sqlx` entry should be readable").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .iter()
        .map(|path| {
            let metadata: Value = serde_json::from_str(
                &fs::read_to_string(path).expect("query metadata should be readable"),
            )
            .expect("query metadata should be JSON");
            let query = metadata["query"]
                .as_str()
                .expect("query metadata should contain a query");

            query.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// Returns the tables or aliases that `query`'s plan scans without an index,
/// ignoring scans of subqueries, CTEs, constants, and virtual tables.
async fn full_scans(pool: &SqlitePool, query: &str) -> Result<Vec<String>, sqlx::Error> {
    let plan = sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!(
        "EXPLAIN QUERY PLAN {query}"
    )))
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(_, _, _, detail)| detail)
    .collect::<Vec<_>>();
    let derived = plan
        .iter()
        .filter_map(|detail| {
            detail
                .strip_prefix("CO-ROUTINE ")
                .or_else(|| detail.strip_prefix("MATERIALIZE "))
        })
        .collect::<BTreeSet<_>>();

    Ok(plan
        .iter()
        .filter_map(|detail| detail.strip_prefix("SCAN "))
        .filter(|scan| !scan.contains(" USING ") && !scan.contains(" VIRTUAL TABLE"))
        .filter(|scan| *scan != "CONSTANT ROW" && !scan.ends_with("VALUES CLAUSE"))
        .filter(|scan| !derived.contains(scan))
        .map(ToString::to_string)
        .collect())
}
