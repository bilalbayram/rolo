//! Webhook delivery worker. Fans events out from the `events` outbox to
//! subscribed webhooks, then POSTs them signed per the Standard Webhooks spec
//! (<https://www.standardwebhooks.com/>), retrying failures with backoff.
//!
//! Run one server per database: the worker assumes it is the only one sending.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use hmac::Hmac;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::CONTENT_TYPE;
use sha2::Sha256;
use sqlx::SqliteConnection;
use sqlx::types::Json;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::error::Result;
use crate::service::events::Event;
use crate::service::webhooks::DeliveryStatus;
use crate::util;

/// Deliveries attempted concurrently per tick.
const DELIVERY_BATCH: i64 = 32;

/// Events scanned per webhook per tick when fanning out.
const FAN_OUT_BATCH: i64 = 1000;

/// When to give up on a delivery, and on an endpoint.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Wait after failed attempt N (1-based) before attempt N + 1. A delivery
    /// gets `backoff.len() + 1` attempts before it is marked failed.
    pub backoff: Vec<Duration>,
    /// Disable a webhook once this many deliveries in a row have failed every
    /// attempt...
    pub disable_after_failed_deliveries: i64,
    /// ...and it has not had a successful delivery for this long.
    pub disable_after_failing_for: Duration,
}

impl Default for RetryPolicy {
    /// 8 attempts over about 21 hours; disable after 5 failed deliveries and
    /// 3 days without success.
    fn default() -> Self {
        Self {
            backoff: vec![
                Duration::from_secs(10),
                Duration::from_secs(60),
                Duration::from_secs(5 * 60),
                Duration::from_secs(30 * 60),
                Duration::from_secs(2 * 60 * 60),
                Duration::from_secs(6 * 60 * 60),
                Duration::from_secs(12 * 60 * 60),
            ],
            disable_after_failed_deliveries: 5,
            disable_after_failing_for: Duration::from_secs(3 * 24 * 60 * 60),
        }
    }
}

impl RetryPolicy {
    /// When to try again after `attempts` failed attempts; `None` to give up.
    fn next_attempt_at(&self, attempts: i64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let index = usize::try_from(attempts - 1).ok()?;
        let delay = self.backoff.get(index)?;
        Some(now + chrono::Duration::from_std(*delay).expect("backoff delays fit in chrono"))
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Allow destinations on loopback, private and other non-public networks.
    /// Off by default so a webhook cannot be used to reach internal services.
    pub allow_private_destinations: bool,
    pub poll_interval: Duration,
    pub retry: RetryPolicy,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            allow_private_destinations: false,
            poll_interval: Duration::from_secs(1),
            retry: RetryPolicy::default(),
        }
    }
}

/// Start the worker. It stops after `shutdown` is cancelled, once the current
/// batch finishes.
pub fn spawn(
    db: Db,
    config: WorkerConfig,
    shutdown: CancellationToken,
) -> anyhow::Result<JoinHandle<()>> {
    let sender = Sender::new(config.allow_private_destinations)?;
    Ok(tokio::spawn(run(db, sender, config, shutdown)))
}

async fn run(db: Db, sender: Sender, config: WorkerConfig, shutdown: CancellationToken) {
    loop {
        if let Err(err) = fan_out(&db).await {
            tracing::error!(error = ?err, "webhook fan-out failed");
        }
        if let Err(err) = deliver_due(&db, &sender, &config.retry).await {
            tracing::error!(error = ?err, "webhook delivery failed");
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(config.poll_interval) => {}
        }
    }
}

/// Create pending deliveries for events past each enabled webhook's cursor.
async fn fan_out(db: &Db) -> Result<()> {
    let behind = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM webhooks
               WHERE enabled = 1 AND cursor_seq < (SELECT coalesce(max(seq), 0) FROM events)
           ) AS "behind!: bool""#,
    )
    .fetch_one(&db.reader)
    .await?;
    if !behind {
        return Ok(());
    }

    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let webhooks = sqlx::query!(
        r#"SELECT id, event_types AS "event_types: Json<Vec<String>>", cursor_seq
           FROM webhooks WHERE enabled = 1"#,
    )
    .fetch_all(&mut *tx)
    .await?;
    for webhook in webhooks {
        let events = sqlx::query!(
            "SELECT seq, type AS event_type FROM events WHERE seq > ? ORDER BY seq LIMIT ?",
            webhook.cursor_seq,
            FAN_OUT_BATCH,
        )
        .fetch_all(&mut *tx)
        .await?;
        let Some(last_seq) = events.last().map(|event| event.seq) else {
            continue;
        };
        let matching = events
            .iter()
            .filter(|event| is_subscribed(&webhook.event_types, &event.event_type));
        for event in matching {
            let id = util::new_id("dlv");
            sqlx::query!(
                "INSERT OR IGNORE INTO webhook_deliveries
                     (id, webhook_id, event_seq, status, next_attempt_at, created_at, updated_at)
                 VALUES (?, ?, ?, 'pending', ?, ?, ?)",
                id,
                webhook.id,
                event.seq,
                now,
                now,
                now,
            )
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query!(
            "UPDATE webhooks SET cursor_seq = ? WHERE id = ?",
            last_seq,
            webhook.id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Empty `prefixes` subscribes to everything.
fn is_subscribed(prefixes: &[String], event_type: &str) -> bool {
    prefixes.is_empty()
        || prefixes
            .iter()
            .any(|prefix| event_type.starts_with(prefix.as_str()))
}

struct DueDelivery {
    id: String,
    webhook_id: String,
    attempts: i64,
    url: String,
    /// The current secret, then the previous one while it is in its grace period.
    secrets: Vec<String>,
    event: Event,
}

struct AttemptResult {
    delivery_id: String,
    webhook_id: String,
    previous_attempts: i64,
    outcome: Outcome,
}

enum Outcome {
    Delivered { status: u16 },
    Failed { status: Option<u16>, error: String },
}

async fn deliver_due(db: &Db, sender: &Sender, policy: &RetryPolicy) -> Result<()> {
    let now = Utc::now();
    let rows = sqlx::query!(
        r#"SELECT d.id, d.webhook_id, d.attempts, w.url, w.secret,
                  CASE WHEN w.previous_secret_expires_at > ?1 THEN w.previous_secret END
                      AS "previous_secret?: String",
                  e.seq, e.id AS event_id, e.type AS event_type, e.entity_id,
                  e.data AS "data: Json<serde_json::Value>",
                  e.occurred_at AS "occurred_at: DateTime<Utc>"
           FROM webhook_deliveries d
           JOIN webhooks w ON w.id = d.webhook_id
           JOIN events e ON e.seq = d.event_seq
           WHERE d.status = 'pending' AND d.next_attempt_at <= ?1 AND w.enabled = 1
           ORDER BY d.next_attempt_at, d.event_seq
           LIMIT ?2"#,
        now,
        DELIVERY_BATCH,
    )
    .fetch_all(&db.reader)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }

    let mut attempts = JoinSet::new();
    for row in rows {
        let mut secrets = vec![row.secret];
        secrets.extend(row.previous_secret);
        let due = DueDelivery {
            id: row.id,
            webhook_id: row.webhook_id,
            attempts: row.attempts,
            url: row.url,
            secrets,
            event: Event {
                seq: row.seq,
                id: row.event_id,
                event_type: row.event_type,
                entity_id: row.entity_id,
                data: row.data,
                occurred_at: row.occurred_at,
            },
        };
        let sender = sender.clone();
        attempts.spawn(async move {
            let outcome = sender.send(&due.url, &due.secrets, &due.event).await;
            AttemptResult {
                delivery_id: due.id,
                webhook_id: due.webhook_id,
                previous_attempts: due.attempts,
                outcome,
            }
        });
    }
    let results = attempts.join_all().await;
    record_results(db, results, policy).await
}

async fn record_results(db: &Db, results: Vec<AttemptResult>, policy: &RetryPolicy) -> Result<()> {
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    for result in results {
        let attempts = result.previous_attempts + 1;
        match result.outcome {
            Outcome::Delivered { status } => {
                let status = i64::from(status);
                sqlx::query!(
                    "UPDATE webhook_deliveries
                     SET status = 'succeeded', attempts = ?, next_attempt_at = NULL,
                         last_status_code = ?, last_error = NULL, delivered_at = ?, updated_at = ?
                     WHERE id = ?",
                    attempts,
                    status,
                    now,
                    now,
                    result.delivery_id,
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query!(
                    "UPDATE webhooks SET failing_since = NULL, consecutive_failed_deliveries = 0
                     WHERE id = ? AND (failing_since IS NOT NULL OR consecutive_failed_deliveries > 0)",
                    result.webhook_id,
                )
                .execute(&mut *tx)
                .await?;
            }
            Outcome::Failed { status, error } => {
                let next_attempt_at = policy.next_attempt_at(attempts, now);
                let delivery_status = match next_attempt_at {
                    Some(_) => DeliveryStatus::Pending,
                    None => DeliveryStatus::Failed,
                };
                let status = status.map(i64::from);
                tracing::warn!(delivery = %result.delivery_id, attempts, %error, "webhook delivery failed");
                sqlx::query!(
                    "UPDATE webhook_deliveries
                     SET status = ?, attempts = ?, next_attempt_at = ?,
                         last_status_code = ?, last_error = ?, updated_at = ?
                     WHERE id = ?",
                    delivery_status,
                    attempts,
                    next_attempt_at,
                    status,
                    error,
                    now,
                    result.delivery_id,
                )
                .execute(&mut *tx)
                .await?;
                let gave_up = next_attempt_at.is_none();
                record_endpoint_failure(&mut tx, &result.webhook_id, gave_up, policy, now).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Track a failed attempt against the endpoint, and disable the webhook if
/// `gave_up` on a delivery pushed it past the policy's thresholds.
async fn record_endpoint_failure(
    conn: &mut SqliteConnection,
    webhook_id: &str,
    gave_up: bool,
    policy: &RetryPolicy,
    now: DateTime<Utc>,
) -> Result<()> {
    let failed_deliveries = i64::from(gave_up);
    sqlx::query!(
        "UPDATE webhooks
         SET failing_since = coalesce(failing_since, ?),
             consecutive_failed_deliveries = consecutive_failed_deliveries + ?
         WHERE id = ?",
        now,
        failed_deliveries,
        webhook_id,
    )
    .execute(&mut *conn)
    .await?;
    if !gave_up {
        return Ok(());
    }

    let failing_for = chrono::Duration::from_std(policy.disable_after_failing_for)
        .expect("disable threshold fits in chrono");
    let cutoff = now - failing_for;
    let disabled = sqlx::query!(
        "UPDATE webhooks
         SET enabled = 0, updated_at = ?1,
             disabled_reason = 'disabled automatically: ' || consecutive_failed_deliveries
                 || ' deliveries in a row failed every attempt, with no successful delivery since '
                 || failing_since
         WHERE id = ?2 AND enabled = 1
           AND consecutive_failed_deliveries >= ?3 AND failing_since <= ?4",
        now,
        webhook_id,
        policy.disable_after_failed_deliveries,
        cutoff,
    )
    .execute(&mut *conn)
    .await?;
    if disabled.rows_affected() > 0 {
        tracing::warn!(webhook = %webhook_id, "webhook disabled after repeated delivery failures");
    }
    Ok(())
}

/// Standard Webhooks signature header value: `v1,<base64 HMAC-SHA256>` over
/// `{message_id}.{timestamp}.{body}`, keyed by the base64 part of the `whsec_` secret.
pub fn sign(secret: &str, message_id: &str, timestamp: i64, body: &[u8]) -> anyhow::Result<String> {
    use base64::Engine as _;
    use hmac::{KeyInit as _, Mac as _};

    let key = BASE64
        .decode(secret.strip_prefix("whsec_").unwrap_or(secret))
        .context("webhook secret is not valid base64")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).context("invalid webhook secret length")?;
    mac.update(format!("{message_id}.{timestamp}.").as_bytes());
    mac.update(body);
    Ok(format!("v1,{}", BASE64.encode(mac.finalize().into_bytes())))
}

#[derive(Clone)]
struct Sender {
    client: reqwest::Client,
    allow_private_destinations: bool,
}

impl Sender {
    fn new(allow_private_destinations: bool) -> anyhow::Result<Self> {
        // reqwest is built without a bundled crypto provider (the default,
        // aws-lc, needs cmake to build), so install ring. This fails harmlessly
        // if a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("rolo/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            // Redirects and proxies would bypass the destination check.
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if !allow_private_destinations {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build().context("building webhook HTTP client")?;
        Ok(Self {
            client,
            allow_private_destinations,
        })
    }

    async fn send(&self, url: &str, secrets: &[String], event: &Event) -> Outcome {
        match self.try_send(url, secrets, event).await {
            Ok(outcome) => outcome,
            Err(err) => Outcome::Failed {
                status: None,
                error: format!("{err:#}"),
            },
        }
    }

    async fn try_send(
        &self,
        url: &str,
        secrets: &[String],
        event: &Event,
    ) -> anyhow::Result<Outcome> {
        if !self.allow_private_destinations
            && let Some(ip) = literal_ip(url)
            && !is_public(ip)
        {
            anyhow::bail!(blocked_message(&ip.to_string()));
        }
        let body = serde_json::to_vec(event)?;
        let timestamp = Utc::now().timestamp();
        // Standard Webhooks allows several space-separated signatures; receivers
        // accept the request if any verifies, which makes rotation seamless.
        let signatures: Vec<String> = secrets
            .iter()
            .map(|secret| sign(secret, &event.id, timestamp, &body))
            .collect::<anyhow::Result<_>>()?;
        let signature = signatures.join(" ");
        let response = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header("webhook-id", &event.id)
            .header("webhook-timestamp", timestamp.to_string())
            .header("webhook-signature", signature)
            .body(body)
            .send()
            .await
            .map_err(|err| anyhow::anyhow!(error_chain(&err)))?;
        let status = response.status();
        if status.is_success() {
            Ok(Outcome::Delivered {
                status: status.as_u16(),
            })
        } else {
            Ok(Outcome::Failed {
                status: Some(status.as_u16()),
                error: format!("endpoint responded {status}"),
            })
        }
    }
}

/// reqwest's top-level error omits the cause (DNS, TLS, connection refused).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    message
}

fn blocked_message(host: &str) -> String {
    format!(
        "blocked: {host} is a private or reserved address; \
         start rolo with --allow-private-webhooks to deliver to internal hosts"
    )
}

/// The URL's host if it is an IP literal (which skips DNS resolution).
fn literal_ip(url: &str) -> Option<IpAddr> {
    let url = reqwest::Url::parse(url).ok()?;
    let host = url.host_str()?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// Resolves hostnames but only returns public addresses. Checking inside the
/// resolver (rather than before the request) means a hostname cannot pass the
/// check and then resolve to an internal address at connect time.
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public: Vec<SocketAddr> =
                resolved.filter(|address| is_public(address.ip())).collect();
            if public.is_empty() {
                return Err(blocked_message(&host).into());
            }
            let addresses: Addrs = Box::new(public.into_iter());
            Ok(addresses)
        })
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match embedded_v4(ip) {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let reserved = a == 0
        // 100.64.0.0/10: carrier-grade NAT
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24: IETF protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15: benchmarking
        || (a == 198 && (18..20).contains(&b))
        // 240.0.0.0/4: reserved
        || a >= 240;
    !(reserved
        || ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast())
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || unique_local
        || link_local
        || documentation)
}

/// IPv4 address carried in an IPv4-mapped (`::ffff:a.b.c.d`) or NAT64
/// (`64:ff9b::a.b.c.d`) IPv6 address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let segments = ip.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., high, low] = segments;
        return Some(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        let public = ["8.8.8.8", "1.1.1.1", "2606:4700::1111", "64:ff9b::808:808"];
        let private = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
        ];
        for address in public {
            assert!(
                is_public(address.parse().expect("valid IP")),
                "{address} should be public"
            );
        }
        for address in private {
            assert!(
                !is_public(address.parse().expect("valid IP")),
                "{address} should be private"
            );
        }
    }

    #[test]
    fn signs_per_standard_webhooks() {
        // Test vector from the Standard Webhooks reference implementation.
        let signature = sign(
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            1614265330,
            br#"{"test": 2432232314}"#,
        )
        .expect("valid secret");
        assert_eq!(signature, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
    }

    #[test]
    fn backs_off_then_gives_up() {
        let policy = RetryPolicy::default();
        let now = Utc::now();
        let after = |attempts| policy.next_attempt_at(attempts, now);
        assert_eq!(after(1), Some(now + chrono::Duration::seconds(10)));
        assert_eq!(after(7), Some(now + chrono::Duration::hours(12)));
        assert_eq!(after(8), None, "8 attempts in total");
    }
}
