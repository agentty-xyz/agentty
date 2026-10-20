# SQLx Offline Metadata

When changing checked queries, regenerate metadata in the owning crate with the SQLx CLI
installed. Use disposable databases under `target/sqlx/`; the commands below reset those
databases. Preserve the committed metadata so checked macros work offline.

## Store Queries

Run from the repository root:

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

## Harness Queries

The harness has its own migrations and metadata cache. Run from the repository root:

```sh
mkdir -p target/sqlx
sqlx_database_url="sqlite://${PWD}/target/sqlx/ag-harness.sqlite"
(
  cd crates/ag-harness
  DATABASE_URL="${sqlx_database_url}" cargo sqlx database reset -y
  DATABASE_URL="${sqlx_database_url}" cargo sqlx prepare -- --all-targets
)
```

Review the changed `.sqlx/` files with the query changes. The committed metadata
directories are `crates/ag-store/.sqlx/`, `crates/ag-harness/.sqlx/`, and
`crates/agentty/.sqlx/`; keep the cache in the crate containing the checked queries.
