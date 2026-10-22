# Backups

All of rolo's state is in one SQLite database file (plus its `-wal` and `-shm`
companions while running). There are two ways to back it up:

- **Snapshots** with `rolo backup`: simple, good for nightly copies.
- **Continuous replication** with [Litestream](https://litestream.io): streams
  every change to S3-compatible storage within about a second, with
  point-in-time restore.

Backups contain everything: CRM data, API key hashes and webhook signing
secrets (stored in plaintext because signing needs them). Keep them in private,
encrypted storage. Litestream 0.5 no longer encrypts replicas itself, so use
server-side encryption on the bucket.

## Snapshots

```sh
rolo --db /var/lib/rolo/rolo.db backup /backups/rolo-$(date +%F-%H%M).db
```

This uses SQLite's `VACUUM INTO`, which writes a consistent, compacted copy
while the server keeps running. Don't `cp` a live database: recent writes may
still be in the `-wal` file, and the copy can be inconsistent.

Nightly via cron, keeping two weeks:

```cron
15 3 * * * rolo --db /var/lib/rolo/rolo.db backup /backups/rolo-$(date +\%F).db && find /backups -name 'rolo-*.db' -mtime +14 -delete
```

To restore:

1. Stop rolo.
2. Replace `rolo.db` with the snapshot and delete any leftover `rolo.db-wal` and `rolo.db-shm`.
3. Start rolo.

Restoring a snapshot taken by an older version is fine: rolo runs pending
migrations at startup.

## Continuous replication with Litestream

These instructions are for Litestream 0.5 or newer, where each database has a
single `replica`.

### Configure

`/etc/litestream.yml`:

```yaml
dbs:
  - path: /var/lib/rolo/rolo.db
    replica:
      url: s3://my-bucket/rolo
      # For S3-compatible storage (Cloudflare R2, Backblaze B2, MinIO, ...):
      # endpoint: https://<account-id>.r2.cloudflarestorage.com
      # region: auto

snapshot:
  interval: 24h
  retention: 168h # keep a week of point-in-time history
```

Credentials come from `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (or
`LITESTREAM_ACCESS_KEY_ID` / `LITESTREAM_SECRET_ACCESS_KEY`).

### Run

Run rolo as a child of Litestream, so replication runs exactly as long as the server does:

```sh
litestream replicate -config /etc/litestream.yml \
  -exec "rolo --db /var/lib/rolo/rolo.db serve"
```

On a fresh machine or container, add `-restore-if-db-not-exists` to pull the
latest replica before starting rolo.

As a systemd service:

```ini
# /etc/systemd/system/rolo.service
[Unit]
Description=rolo CRM with Litestream replication
After=network-online.target
Wants=network-online.target

[Service]
User=rolo
# Contains AWS_ACCESS_KEY_ID=... and AWS_SECRET_ACCESS_KEY=...
EnvironmentFile=/etc/rolo/env
ExecStart=/usr/local/bin/litestream replicate -config /etc/litestream.yml \
  -restore-if-db-not-exists \
  -exec "/usr/local/bin/rolo --db /var/lib/rolo/rolo.db serve --addr 127.0.0.1:3000"
Restart=always

[Install]
WantedBy=multi-user.target
```

### Restore

Latest state, into a new file:

```sh
litestream restore -config /etc/litestream.yml -o /tmp/rolo-restored.db /var/lib/rolo/rolo.db
```

Point in time, e.g. just before an accidental bulk delete:

```sh
litestream restore -config /etc/litestream.yml -timestamp 2026-10-22T09:00:00Z \
  -o /tmp/rolo-restored.db /var/lib/rolo/rolo.db
```

Inspect the restored file, for example with `sqlite3 /tmp/rolo-restored.db "PRAGMA integrity_check"`
or by pointing a second rolo at it on another port. Then stop the service, move it
into place (deleting any `-wal`/`-shm` files), and start the service again.

### Rules

- **One replicator per destination.** Never let two machines replicate to the
  same bucket path; it can make restores impossible.
- **One `rolo serve` per database.** The webhook worker assumes it is the only
  sender.
- **rolo already uses the settings Litestream recommends:** WAL journal mode,
  `busy_timeout = 5000` and `synchronous = NORMAL`. Litestream also suggests
  disabling automatic checkpoints for very high write loads. A CRM doesn't
  need that, so rolo leaves them on.
- **Test restores regularly.** A backup you have never restored is a hope, not a backup.
