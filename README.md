# rolo

A headless CRM for agents and apps. One binary, one SQLite file, two interfaces
over the same data:

- **REST API** at `/v1` with a generated OpenAPI spec (`/openapi.json`, browsable at `/docs`)
- **MCP server** at `/mcp` (streamable HTTP) with tools curated for AI agents

Every mutation is also appended to a change feed (`/v1/events`) in the same
transaction and delivered to signed webhooks, so integrations can sync
without missing anything.

MIT licensed.

## Quick start

```sh
cargo build --release
./target/release/rolo keys create --name me          # prints an API key once
./target/release/rolo serve                          # http://127.0.0.1:3000
```

```sh
TOKEN=rolo_...
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"name": "Acme", "domain": "acme.com"}' http://127.0.0.1:3000/v1/companies
```

Configuration (flags or env vars):

| Flag | Env | Default |
|---|---|---|
| `--db` | `ROLO_DB` | `rolo.db` |
| `--addr` | `ROLO_ADDR` | `127.0.0.1:3000` |
| `--allowed-host` | `ROLO_ALLOWED_HOSTS` (comma-separated) | loopback only |
| `--allow-private-webhooks` | `ROLO_ALLOW_PRIVATE_WEBHOOKS` | off |

`--allowed-host` guards the MCP endpoint against DNS rebinding. Set it to your
public hostname when running rolo behind a domain.

Run one `rolo serve` per database file, because the webhook worker assumes it
is the only one sending.

## Connect an agent

Any MCP client that supports streamable HTTP and custom headers works. For Claude Code:

```sh
claude mcp add --transport http rolo http://127.0.0.1:3000/mcp \
  --header "Authorization: Bearer $TOKEN"
```

Tools: `search`, `list_users`, `search_contacts`, `get_contact`, `create_contact`, `update_contact`,
`search_companies`, `get_company`, `create_company`, `update_company`,
`list_pipelines`, `search_deals`, `get_deal`, `create_deal`, `update_deal`,
`list_activities`, `log_activity`, `update_activity`.

The `get_*` tools return a record with its context (company, deals, recent
activity) so an agent needs fewer round trips. There are no delete tools on
purpose; deletes are REST-only.

## API keys and users

```sh
rolo users create --name "Ada Lovelace" --email ada@example.com   # prints usr_...
rolo keys create --name ada-agent --user usr_...    # key acting as Ada
rolo keys create --name zapier --scope read         # read | write (default); no user
rolo keys list
rolo keys revoke key_01...
```

Keys are stored as SHA-256 hashes. `read` keys can call GET endpoints and
read-only MCP tools; mutations return `403 forbidden`.

Contacts, companies, deals and activities have an `owner_id` (for tasks, the
assignee):

- **Default owner:** a record created with a key that belongs to a user is owned by that user, unless the request sets `owner_id` (`null` means no owner).
- **`"me"`** means the key's user anywhere an owner is accepted, including filters. For example, `GET /v1/activities?owner_id=me&open=true` lists your open tasks, and `owner_id=none` finds unowned records.
- **Deactivating a user** (`PATCH /v1/users/{id} {"deactivated": true}`) keeps their records, blocks assigning new ones to them, and disables their keys. Deleting a user unowns their records and revokes their keys.
- **For agents:** `GET /v1/users/me` and the `list_users` MCP tool tell an agent who it is acting as.

## Data model

| Record | ID prefix | Notes |
|---|---|---|
| Company | `com_` | `domain` is unique (case-insensitive) |
| Contact | `con_` | `email` is unique (case-insensitive); optional `company_id` |
| Pipeline / Stage | `pip_` / `stg_` | stages are `open`, `won` or `lost`; a default "Sales" pipeline is created |
| Deal | `deal_` | `amount` in minor units (cents); entering a won/lost stage sets `closed_at` |
| Activity | `act_` | `note`, `call`, `email`, `meeting` or `task`; links to a contact, company and/or deal |
| User | `usr_` | owns records; `email` is unique; can be deactivated |

- IDs are ULIDs, so they sort by creation time.
- Every record has `custom`, a free-form JSON object. PATCH merges it, and setting a key to `null` removes that key.
- PATCH changes only the fields you send. Setting a nullable field to `null` clears it.
- Lists use cursor pagination: `?limit=50&cursor=<next_cursor>`. Their `q` filter uses the search index (see below).
- Errors look like `{"error": {"code": "conflict", "message": "..."}}`.
- Deleting a record keeps the activities linked to it and just unlinks them, so a deal keeps its history.

## Search

```sh
curl -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:3000/v1/search?q=acme+rob&types=company,contact"
```

Full-text search (SQLite FTS5) across contacts, companies, deals and activities:

- **What's searched:** names, emails, phones, domains, deal titles, activity subjects and bodies, and custom field values.
- **How words match:** each word matches the start of a word (`acm` finds Acme), and all words must match. Accents are ignored, so `zoe` finds Zoë, and punctuation is ignored, so `ada@acme.io` works.
- **Ranking:** matches in titles (names, deal titles, activity subjects) rank highest. Each hit includes a snippet with the matched words in `**bold**`.
- **List filters:** `q` on the list endpoints uses the same index but keeps the usual ordering and pagination.

## Change feed

```sh
curl -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:3000/v1/events?after=0&type=deal."
```

Events (`contact.created`, `deal.stage_changed`, `company.deleted`, ...) carry a
snapshot of the record. Store `next_after` and pass it as `after` on the next poll.

## Webhooks

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"url": "https://example.com/hooks/rolo", "event_types": ["deal.", "contact.created"]}' \
     http://127.0.0.1:3000/v1/webhooks
```

- **Payload:** each event is POSTed as JSON, in the same shape as `/v1/events`. A new webhook gets events from the moment it's created, not history. `event_types` is a list of type prefixes; leave it empty to get everything.
- **Signing:** requests follow [Standard Webhooks](https://www.standardwebhooks.com/). The response that creates a webhook includes a `whsec_...` secret, shown only once. Verify the `webhook-signature` header against `webhook-id`, `webhook-timestamp` and the raw body, using any Standard Webhooks library. `webhook-id` stays the same across retries, so you can use it to ignore duplicates.
- **Retries:** any non-2xx response, or no response within 10s, is retried with backoff (10s, 1m, 5m, 30m, 2h, 6h, 12h), 8 attempts in total over about 21 hours. Delivery order is not guaranteed; use `seq` or `/v1/events` if order matters.
- **History and replays:** `GET /v1/webhooks/{id}/deliveries` lists attempts and errors, and `POST .../deliveries/{delivery_id}/retry` re-sends one.
- **Pausing:** `PATCH {"enabled": false}` pauses a webhook. Re-enabling it delivers everything it missed.
- **Failing endpoints are disabled automatically** once 5 deliveries in a row have failed every attempt *and* the endpoint has had no successful delivery for 3 days. `disabled_reason` says why, and `failing_since` and `consecutive_failed_deliveries` show its health. Re-enable it with `PATCH {"enabled": true}` to deliver what it missed.
- **Rotating secrets:** `POST /v1/webhooks/{id}/rotate-secret?grace_period_hours=24` returns a new secret. For the grace period (0–168 hours), each request carries two signatures, from the new and the previous secret, so receivers can switch with no rejected deliveries.
- **Security:**
  - Destinations on loopback, private, link-local and other internal ranges are blocked, unless you start the server with `--allow-private-webhooks`.
  - Hostnames are checked as they're resolved, so DNS tricks can't get around the block.
  - Redirects are not followed.
  - Webhooks can only be managed over REST with a write key. There are no MCP tools for them, so an agent can't create a way to send your data out.

## Backups

The whole CRM is one SQLite file. `rolo backup <file>` writes a consistent
snapshot while the server keeps running. For continuous replication to
S3-compatible storage with point-in-time restore, run rolo under
[Litestream](https://litestream.io). See [docs/backup.md](docs/backup.md).

## Development

Code follows the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html).
Its rustfmt settings (`rustfmt.toml`) include unstable options, so format with
nightly rustfmt (`rustup toolchain install nightly --profile minimal --component rustfmt`):

```sh
cargo +nightly fmt
cargo clippy --all-targets
```

Queries are checked at compile time against the schema by `sqlx`. The checked-in
`.sqlx/` directory lets the project build without a database. When you change
queries or migrations:

```sh
cargo install sqlx-cli --no-default-features --features sqlite
sqlx database setup          # creates dev.db from migrations (DATABASE_URL in .env)
cargo sqlx prepare -- --all-targets
cargo test
```

Layout: `src/service/` holds the business logic, and every write there also
records an event. `src/api/` (REST) and `src/mcp.rs` (MCP) are thin wrappers
over it. `src/delivery.rs` is the webhook worker. Migrations live in `migrations/`.

## Roadmap

- Custom field definitions (typed and validated) on top of `custom`
