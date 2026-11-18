//! The migrated schema follows the persistence naming conventions.

use ag_store::Database;
use sqlx::SqlitePool;

/// Legacy schema items that predate a schema convention, as
/// `(table, column, reason)`, with an empty column for a table name. Add
/// entries only for items that already ship; fix new items in a migration
/// instead.
const NAMING_EXCEPTIONS: &[(&str, &str, &str)] = &[
    (
        "project",
        "is_favorite",
        "added in migration 025 without a boolean CHECK constraint",
    ),
    (
        "session",
        "has_diff",
        "added in migration 061 as a nullable flag without a boolean CHECK constraint",
    ),
    (
        "session",
        "is_draft",
        "added in migration 038 without a boolean CHECK constraint",
    ),
    (
        "session",
        "is_title_provisional",
        "added in migration 063 without a boolean CHECK constraint",
    ),
];

/// Naming problems and the number of constrained columns inspected.
#[derive(Default)]
struct Audit {
    boolean_columns: usize,
    foreign_keys: usize,
    violations: Vec<String>,
}

impl Audit {
    async fn inspect_table(
        &mut self,
        pool: &SqlitePool,
        table: &str,
        sql: &str,
    ) -> Result<(), sqlx::Error> {
        if !is_snake_case(table) {
            self.report(table, "", "table name is not snake_case");
        }

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(table)
                .fetch_all(pool)
                .await?;
        for column in &columns {
            if !is_snake_case(column) {
                self.report(table, column, "column name is not snake_case");
            }

            let has_boolean_prefix = column.starts_with("is_") || column.starts_with("has_");
            if is_boolean_column(sql, column) {
                self.boolean_columns += 1;
                if !has_boolean_prefix {
                    self.report(
                        table,
                        column,
                        "boolean column must start with `is_` or `has_`",
                    );
                }
            } else if has_boolean_prefix {
                self.report(
                    table,
                    column,
                    "boolean column must declare `CHECK (<column> IN (0, 1))`",
                );
            }
        }

        // Composite keys name each column after its referenced column instead.
        let foreign_keys: Vec<(String, String)> = sqlx::query_as(
            "SELECT \"from\", \"table\" FROM pragma_foreign_key_list(?1) WHERE id IN (SELECT id \
             FROM pragma_foreign_key_list(?1) GROUP BY id HAVING COUNT(*) = 1)",
        )
        .bind(table)
        .fetch_all(pool)
        .await?;
        for (column, referenced_table) in foreign_keys {
            self.foreign_keys += 1;
            if !column.ends_with(&format!("{referenced_table}_id")) {
                let problem = format!("foreign key must be named `[<role>_]{referenced_table}_id`");
                self.report(table, &column, &problem);
            }
        }

        Ok(())
    }

    fn report(&mut self, table: &str, column: &str, problem: &str) {
        let is_excepted = NAMING_EXCEPTIONS
            .iter()
            .any(|(excepted_table, excepted_column, _)| {
                *excepted_table == table && *excepted_column == column
            });
        if !is_excepted {
            self.violations.push(format!("{table}.{column}: {problem}"));
        }
    }
}

#[tokio::test]
async fn migrated_schema_follows_naming_conventions() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("in-memory database should open");
    let pool = database.pool();
    let tables: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         AND name NOT LIKE '_sqlx_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("schema tables should load");
    let mut audit = Audit::default();

    // Act
    for (table, sql) in &tables {
        audit
            .inspect_table(pool, table, sql)
            .await
            .expect("table schema should load");
    }

    // Assert
    assert!(
        audit.boolean_columns > 0 && audit.foreign_keys > 0,
        "the audit found no boolean columns or foreign keys; update its schema parsing"
    );
    assert!(
        audit.violations.is_empty(),
        "Schema naming violations; rename them in a new migration, or add a legacy name to \
         `NAMING_EXCEPTIONS` with a reason:\n{}",
        audit.violations.join("\n")
    );
}

#[test]
fn naming_exceptions_have_reasons() {
    // Arrange
    let exceptions = NAMING_EXCEPTIONS;

    // Act
    let missing = exceptions
        .iter()
        .filter(|(_, _, reason)| reason.trim().is_empty())
        .count();

    // Assert
    assert_eq!(missing, 0, "every naming exception needs a reason");
}

fn is_snake_case(name: &str) -> bool {
    let mut characters = name.chars();

    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Returns whether the table declares `CHECK (<column> IN (0, 1))`.
fn is_boolean_column(sql: &str, column: &str) -> bool {
    let compact = sql
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    compact.contains(&format!("check({column}in(0,1))"))
}
