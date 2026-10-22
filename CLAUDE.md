# rolo: notes for agents

Headless, MIT-licensed CRM for agents and apps: a REST API (`/v1`, OpenAPI at
`/openapi.json`), an MCP server (`/mcp`), a change feed (`/v1/events`) and signed
webhooks, all over one SQLite file. Rust (edition 2024): Axum, sqlx (SQLite),
rmcp, utoipa. See README.md for the product surface and docs/backup.md for operations.

## Workflow

- Commit directly on `main`; no feature branches. Only commit or push when asked.
- Commit messages end with a `Co-Authored-By:` trailer for the model.
- Public repo: https://github.com/bilalbayram/rolo

## Style

Follow the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html):

- **Imports:** one module per `use` statement (`imports_granularity = Module`).
- **Calls:** call free functions through their module, e.g. `util::non_empty(..)`.
- **Modules:** non-leaf modules (`mod.rs`) hold only declarations, re-exports and small glue.
- **Panics:** use `expect("reason")`, never `unwrap()`, outside tests. Never reach into a poisoned mutex.
- **Naming:** no abbreviated names. Use enums instead of bool parameters when the meaning isn't obvious.
- **Types:** use a newtype when a type has its own API. Import a trait used only once locally (`use Trait as _`).
- **Formatting:** format with `cargo +nightly fmt`, since `rustfmt.toml` sets unstable options. Nightly rustfmt is installed.

## Checks before committing

```sh
cargo +nightly fmt
cargo clippy --all-targets          # must be clean
cargo test                          # integration tests live in tests/, shared harness in tests/common
cargo sqlx prepare -- --all-targets # after changing any query or migration; commit .sqlx/
```

- **Database:** queries use compile-time-checked `sqlx::query!` macros against `dev.db` (`DATABASE_URL` in `.env`). After adding a migration, run `sqlx migrate run` to update `dev.db`.
- **Build profile:** the dev profile is slimmed (little debug info) to save disk; `target/` still grows past 3 GB. `cargo clean` is fine.
- **Parallel builds:** the machine has 4 cores and about 7.5 GB RAM with no swap, so use `-j 3` for big builds.

## Architecture

- **`src/service/`:** business logic. Every CRM mutation runs in one write transaction that also appends to the `events` outbox via `events::record`. Services that create, update or list owned records take the caller (`&Principal`).
- **`src/api/`:** REST handlers, thin wrappers over services. `extract.rs` provides `Body`, `Params` and `Id`, extractors that return the JSON error format.
- **Idempotency (`src/api/idempotency.rs`):** middleware on the REST router, after auth. A POST with `Idempotency-Key` stores its response in `idempotency_keys` (per API key, 24 hours) and replays it on retries. Handlers need nothing extra.
- **`src/mcp.rs`:** MCP tools, also thin wrappers. There are no delete tools and no tools to manage webhooks or definitions, on purpose.
- **`src/delivery.rs`:** the webhook worker.
- **Database (`src/db.rs`):** a single-connection writer pool plus a read-only reader pool. Run one `rolo serve` per database.
- **Phones (`src/phone.rs`):** contact phones are stored in E.164 and unique when present. Contact services take the default `Region` (`--default-region`, in `AppState`) to read local numbers.
- **IDs:** prefixed monotonic ULIDs (`con_`, `com_`, `deal_`, `act_`, `usr_`, ...); list ordering and cursors rely on them.
- **Migrations** in `migrations/` are append-only: never edit a released migration.

## Tools available

- **rikyu MCP** (https://rikyu.ai/mcp, user scope, OAuth already done) generates logos and designs and uses the user's Rikyū credits. Read `read_guide` before requesting. For logos, settle the form first, make the symbol before the matching wordmark, and choose colors last.
- **GitHub CLI** (`gh`) is logged in as `bilalbayram`.
