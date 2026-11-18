---
name: regenerate-sqlx-metadata
description: Regenerate committed SQLx offline query metadata for ag-store, ag-harness, or agentty after changing checked queries or migrations, using disposable databases under target/sqlx/.
---

# Regenerate SQLx Metadata

Use this skill when a change adds, edits, or removes a checked SQLx query macro, or a
migration the checked queries depend on. Install the SQLx CLI first. The commands below
reset disposable databases under `target/sqlx/`; never point them at a real database.
Preserve the committed metadata so checked macros keep working offline.

## Steps

1. Identify the crate that owns the changed queries. The committed metadata directories
   are `crates/ag-store/.sqlx/`, `crates/ag-harness/.sqlx/`, and
   `crates/agentty/.sqlx/`; keep the cache in the crate containing the checked queries.

1. For store queries, run from the repository root:

   ```sh
   mkdir -p target/sqlx
   sqlx_database_url="sqlite://${PWD}/target/sqlx/ag-store.sqlite"
   (
     cd crates/ag-store
     DATABASE_URL="${sqlx_database_url}" cargo sqlx database reset -y
     AGENTTY_STORE_DATABASE_URL="${sqlx_database_url}" cargo sqlx prepare -- --all-targets --all-features
   )
   ```

   The store uses a crate-specific database variable so its native-harness dependencies
   continue to compile against their own offline metadata during preparation.

1. For harness queries, which have their own migrations and metadata cache, run from the
   repository root:

   ```sh
   mkdir -p target/sqlx
   sqlx_database_url="sqlite://${PWD}/target/sqlx/ag-harness.sqlite"
   (
     cd crates/ag-harness
     DATABASE_URL="${sqlx_database_url}" cargo sqlx database reset -y
     DATABASE_URL="${sqlx_database_url}" cargo sqlx prepare -- --all-targets
   )
   ```

1. For `agentty` queries, which are test and E2E support queries checked against the
   store schema, run from the repository root. Also run this step after a store
   migration change, because those queries depend on the store tables:

   ```sh
   mkdir -p target/sqlx
   sqlx_database_url="sqlite://${PWD}/target/sqlx/agentty.sqlite"
   (
     cd crates/ag-store
     DATABASE_URL="${sqlx_database_url}" cargo sqlx database reset -y
   )
   (
     cd crates/agentty
     AGENTTY_STORE_DATABASE_URL="${sqlx_database_url}" cargo sqlx prepare -- --all-targets
   )
   ```

   `crates/agentty/sqlx.toml` reads the same store variable, so leave `DATABASE_URL`
   unset here; otherwise the harness queries compiled as a dependency are checked
   against the store database and fail.

1. Review the changed `.sqlx/` files together with the query changes, then validate
   through `skills/validate-changes/SKILL.md`.

## Handoff

Report the crates whose metadata was regenerated, the `.sqlx/` files added, changed, or
removed, and the checks run.
