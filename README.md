<p align="center">
  <img src="assets/logo/icon.svg" width="96" alt="">
</p>
<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/wordmark-dark.svg">
    <img src="assets/logo/wordmark.svg" height="48" alt="rolo">
  </picture>
</h1>

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
| `--default-region` | `ROLO_DEFAULT_REGION` | `TR` |

`--allowed-host` guards the MCP endpoint against DNS rebinding. Set it to your
public hostname when running rolo behind a domain.

`--default-region` is the country (ISO 3166-1 alpha-2 code) used to read phone
numbers written without a country code. See [Phone numbers](#phone-numbers).

Run one `rolo serve` per database file, because the webhook worker assumes it
is the only one sending.

## Connect an agent

Any MCP client that supports streamable HTTP and custom headers works. For Claude Code:

```sh
claude mcp add --transport http rolo http://127.0.0.1:3000/mcp \
  --header "Authorization: Bearer $TOKEN"
```

Tools:

- **Find:** `search`, `search_contacts`, `search_companies`, `search_deals`, `list_activities`
- **Read:** `get_contact`, `get_company`, `get_deal`
- **Write:** `upsert_contact`, `create_contact`, `update_contact`, `create_company`, `update_company`, `create_deal`, `update_deal`, `log_activity`, `update_activity`
- **Reference:** `list_pipelines`, `list_users`, `list_custom_fields`

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
| Contact | `con_` | `email` is unique (case-insensitive); `phone` is stored in E.164 form and unique; optional `company_id` |
| Pipeline / Stage | `pip_` / `stg_` | stages are `open`, `won` or `lost`; a default "Sales" pipeline is created |
| Deal | `deal_` | `amount` in minor units (cents); entering a won/lost stage sets `closed_at` |
| Activity | `act_` | `note`, `call`, `email`, `meeting` or `task`; links to a contact, company and/or deal |
| User | `usr_` | owns records; `email` is unique; can be deactivated |

- IDs are ULIDs, so they sort by creation time.
- **Importing:** creating a contact, company, deal or activity, including through an upsert that creates, accepts an optional `created_at` (RFC 3339, not in the future), so imported records keep their original dates. `updated_at` gets the same value, and the ID is generated from that time, so the record sorts where it would have. A deal imported into a won or lost stage is closed at that time, and an activity's `occurred_at` defaults to it. Events still carry the real time of the import.
- Every record has `custom`, a JSON object for extra fields. PATCH merges it, and setting a key to `null` removes that key. Keys can be given typed definitions (see below).
- PATCH changes only the fields you send. Setting a nullable field to `null` clears it.
- Lists use cursor pagination: `?limit=50&cursor=<next_cursor>`. Their `q` filter uses the search index (see below).
- Contact, company, deal and activity lists add `"total"` (the number of records matching all filters, across pages) when you pass `include_total=true`, e.g. for "page 2 of 8". It costs an extra count query, so it's off by default and the field is omitted.
- Fetch specific contacts, companies or deals with `?ids=con_a,con_b` (at most 100). Matches come back in the order given, in one page; unknown IDs are left out, and `limit`, `cursor`, `sort` and `include_total` can't be combined with it.
- Contacts, companies and deals list oldest first by default (`sort=id`); `sort=-id` lists newest first, and the cursor follows whichever order you chose. Activities always list newest first.
- Deals can be filtered by expected close date, with both ends inclusive: `GET /v1/deals?expected_close_from=2026-11-01&expected_close_to=2026-11-30`. They can also be sorted with `sort=expected_close_date` (soonest first) or `sort=-expected_close_date` (latest first), with deals without a date last. With a date sort, `next_cursor` is `<date>/<id>` (`none/<id>` for an undated deal), so pagination stays stable when dates tie.
- Errors look like `{"error": {"code": "conflict", "message": "..."}}`.
- Unknown query parameters are rejected with `422` (`unknown query parameter "ownr_id"`), so a mistyped filter fails loudly instead of returning everything. MCP tools reject unknown arguments the same way.
- Deleting a record keeps the activities linked to it and just unlinks them, so a deal keeps its history.

## Safe retries

Every REST `POST` accepts an `Idempotency-Key` header, so a client can retry
after a timeout without creating a record twice:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -H 'Idempotency-Key: dm-lead-8f2c' \
     -d '{"first_name": "Ayşe", "phone": "0532 123 45 67"}' http://127.0.0.1:3000/v1/contacts
```

- **Replays:** repeating the same request (method, path, query and body) with the same key returns the stored response, status included, with the header `Idempotent-Replayed: true`. Error responses below 500 are replayed too; after a 5xx the key is released so the retry runs again.
- **Misuse:** reusing a key for a different request returns `422`. Sending it again while the first request is still running returns `409`.
- **Scope and lifetime:** keys are 1–255 characters, scoped per API key, and kept for 24 hours. A request runs to completion even if the client disconnects, so its retry finds the response.

## Upserting contacts

`POST /v1/contacts/upsert` records a person without creating duplicates. It's
the call to use for leads from forms, chats and other integrations:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"first_name": "Ayşe", "phone": "0532 123 45 67", "custom": {"source": "instagram_dm"}}' \
     http://127.0.0.1:3000/v1/contacts/upsert
# 201 {"contact": {...}, "created": true}, then 200 {"contact": {...}, "created": false}
```

- **Body:** the fields of `POST /v1/contacts`, plus an optional `match_on`, a list of `"email"`, `"phone"` and `"custom.<key>"` (default `["email", "phone"]`).
- **Matching:** keys are tried in order. The first key that has a value in the body and matches a contact wins. `custom.<key>` needs a custom field defined with `unique: true`, such as `instagram_id`.
- **When a contact matches,** only the fields the body sets to a non-blank value change. `custom` is merged as in PATCH, and a null `owner_id` leaves the owner alone. The response is `200` with `created: false`.
- **When none matches,** the contact is created as with `POST /v1/contacts`, and the response is `201` with `created: true`.
- **Conflicts:** if the body matches one contact by email but its phone belongs to another, it returns `409`.
- **For agents:** the MCP tool is `upsert_contact`, and the server instructions recommend it for recording people.

`POST /v1/deals/upsert` does the same for deals that carry an external ID in
a unique custom field, such as a conversation or an imported inquiry:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"title": "Düğün · Ayşe", "custom": {"conversation_id": "c_81"}, "match_on": ["custom.conversation_id"]}' \
     http://127.0.0.1:3000/v1/deals/upsert
# 201 {"deal": {...}, "created": true}, then 200 {"deal": {...}, "created": false}
```

- **Body:** the fields of `POST /v1/deals`, with `title` needed only when creating, plus a required `match_on` list of `"custom.<key>"` for unique custom fields.
- **When a deal matches,** the fields the body sets change and `custom` is merged. A new `stage_id` moves the deal as with PATCH, which sets or clears `closed_at` and records `deal.stage_changed`. `pipeline_id` applies only when creating.

## Phone numbers

Contact phone numbers are normalized to [E.164](https://en.wikipedia.org/wiki/E.164)
with libphonenumber's metadata (the `phonenumber` crate):

- **Local numbers** are read in the default region: with `TR`, `0532 123 45 67` and `532 123 45 67` become `+905321234567`. Numbers with a country code (`+44 20 7946 0958`, `0044 20 7946 0958`) keep it.
- **Invalid numbers** are rejected with `422`, e.g. `invalid phone number "call me": use international format like +905321234567, or a number valid in TR`.
- **Unique when present:** a second contact with the same number gets `409` with `a contact with this phone already exists`. Phone is not an identity key; contacts without a phone never clash.
- **Exact lookup:** `GET /v1/contacts?phone=0532%20123%2045%2067` finds `+905321234567`; the filter is normalized the same way.
- **Older data:** `rolo serve` rewrites numbers stored before normalization at startup. A number that is invalid, or that another contact already has, moves to `custom.legacy_phone`, so nothing is lost.

## Custom fields

`custom` accepts any keys. To make a key typed and validated, define it:

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"entity_type": "contact", "key": "tier", "field_type": "select", "options": ["gold", "silver"], "required": true}' \
     http://127.0.0.1:3000/v1/custom-fields
```

- **Types:** `text`, `number`, `boolean`, `date` (`YYYY-MM-DD`), `datetime` (RFC 3339, stored in UTC), `url` (http/https), `email` (stored lowercased), `select` (one of `options`) and `multiselect` (an array of distinct `options`).
- **Validation:** values for defined keys are checked and normalized on every create and update. All problems are reported in one `422`, e.g. `custom.tier: must be one of: gold, silver; custom.nps: must be a number`.
- **Required fields** must be set when a record is created, and can't be removed later.
- **Unique fields** (`"unique": true`) can't repeat a value across records of the same type, e.g. an `instagram_id` on contacts. A clash returns `409`, e.g. `custom.instagram_id: another contact has this value`. `unique` is set when the field is defined and can't be changed later. It isn't allowed for `boolean` and `multiselect` fields, or when existing records already share a value.
- **Filtering:** contact and deal lists match defined fields exactly with `custom.<key>=<value>` parameters, e.g. `GET /v1/deals?custom.event_type=dugun_nisan&custom.mihrabat_inquiry_id=123`. Several filters must all match. Values are normalized as on write, so `120` matches `120.0`, and keys without a definition, multiselect fields and repeated keys return `422`. The `search_contacts` and `search_deals` MCP tools take the same filters as `custom: {"key": value}`.
- **Changes don't apply retroactively.** Existing records aren't revalidated when you change a definition. Deleting a definition keeps the stored values; the key just becomes free-form again. Keys without a definition are always accepted.
- **For agents:** the `list_custom_fields` MCP tool shows an agent the available fields and their options.

## Search

```sh
curl -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:3000/v1/search?q=acme+rob&types=company,contact"
```

Full-text search (SQLite FTS5) across contacts, companies, deals and activities:

- **What's searched:** names, emails, phones, domains, deal titles, activity subjects and bodies, and custom field values.
- **How words match:** each word matches the start of a word (`acm` finds Acme), and all words must match. Accents are ignored, so `zoe` finds Zoë, and Turkish `ı` and `İ` match `i`, so `isik`, `Işık` and `IŞIK` all find Işık. Punctuation is ignored, so `ada@acme.io` works.
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
