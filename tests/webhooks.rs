mod common;

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use common::{Destinations, TestApp};
use serde_json::{Value, json};

/// A local HTTP endpoint that records webhook requests.
#[derive(Clone)]
struct Receiver {
    url: String,
    requests: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    status: Arc<AtomicU16>,
}

impl Receiver {
    async fn start() -> Self {
        let receiver = Receiver {
            url: String::new(),
            requests: Arc::default(),
            status: Arc::new(AtomicU16::new(200)),
        };
        let router = Router::new()
            .route("/hook", post(record))
            .with_state(receiver.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await });
        Receiver {
            url: format!("http://127.0.0.1:{port}/hook"),
            ..receiver
        }
    }

    fn respond_with(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }

    fn requests(&self) -> Vec<(HeaderMap, Bytes)> {
        self.requests.lock().unwrap().clone()
    }
}

async fn record(State(receiver): State<Receiver>, headers: HeaderMap, body: Bytes) -> StatusCode {
    receiver.requests.lock().unwrap().push((headers, body));
    StatusCode::from_u16(receiver.status.load(Ordering::SeqCst)).unwrap()
}

/// Poll `check` until it returns `Some`, failing after a few seconds.
async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    for _ in 0..250 {
        if let Some(value) = check().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn create_webhook(app: &TestApp, body: Value) -> Value {
    let (status, webhook) = app.post("/v1/webhooks", body).await;
    assert_eq!(status, StatusCode::CREATED, "{webhook}");
    webhook
}

/// Wait for the webhook's newest delivery to satisfy `done`.
async fn delivery_where(app: &TestApp, webhook_id: &str, done: impl Fn(&Value) -> bool) -> Value {
    let uri = format!("/v1/webhooks/{webhook_id}/deliveries");
    eventually("delivery state", || async {
        let (_, page) = app.get(&uri).await;
        page["data"]
            .get(0)
            .filter(|delivery| done(delivery))
            .cloned()
    })
    .await
}

fn expected_signature(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> String {
    use base64::Engine as _;
    use hmac::{KeyInit as _, Mac as _};

    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine
        .decode(secret.strip_prefix("whsec_").unwrap())
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key).unwrap();
    mac.update(format!("{id}.{timestamp}.").as_bytes());
    mac.update(body);
    format!("v1,{}", engine.encode(mac.finalize().into_bytes()))
}

#[tokio::test]
async fn delivers_signed_events_matching_its_filter() {
    let app = TestApp::with_worker(Destinations::AllowPrivate).await;
    let receiver = Receiver::start().await;
    app.post("/v1/contacts", json!({"first_name": "Before"}))
        .await;

    let webhook = create_webhook(
        &app,
        json!({"url": receiver.url, "event_types": ["contact."]}),
    )
    .await;
    let secret = webhook["secret"].as_str().unwrap();
    assert!(secret.starts_with("whsec_"));
    let webhook_id = webhook["id"].as_str().unwrap();
    let (_, fetched) = app.get(&format!("/v1/webhooks/{webhook_id}")).await;
    assert!(
        fetched.get("secret").is_none(),
        "secret is only shown on creation"
    );

    app.post("/v1/companies", json!({"name": "Not subscribed"}))
        .await;
    let (_, contact) = app.post("/v1/contacts", json!({"first_name": "Ada"})).await;

    let delivery = delivery_where(&app, webhook_id, |d| d["status"] == "succeeded").await;
    assert_eq!(delivery["attempts"], 1);
    assert_eq!(delivery["last_status_code"], 200);

    let requests = receiver.requests();
    assert_eq!(
        requests.len(),
        1,
        "only events after creation and matching the filter"
    );
    let (headers, body) = &requests[0];
    let event: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(event["type"], "contact.created");
    assert_eq!(event["entity_id"], contact["id"]);
    assert_eq!(event["data"]["first_name"], "Ada");

    let header = |name: &str| headers[name].to_str().unwrap().to_string();
    assert_eq!(header("webhook-id"), event["id"].as_str().unwrap());
    assert_eq!(
        header("webhook-signature"),
        expected_signature(
            secret,
            &header("webhook-id"),
            &header("webhook-timestamp"),
            body
        )
    );
}

#[tokio::test]
async fn retries_failed_deliveries() {
    let app = TestApp::with_worker(Destinations::AllowPrivate).await;
    let receiver = Receiver::start().await;
    receiver.respond_with(500);
    let webhook = create_webhook(&app, json!({"url": receiver.url})).await;
    let webhook_id = webhook["id"].as_str().unwrap();

    app.post("/v1/companies", json!({"name": "Acme"})).await;
    let failed = delivery_where(&app, webhook_id, |d| d["attempts"] == 1).await;
    assert_eq!(
        failed["status"], "pending",
        "a failed attempt is retried later"
    );
    assert_eq!(failed["last_status_code"], 500);
    assert!(failed["next_attempt_at"].is_string());

    receiver.respond_with(204);
    let delivery_id = failed["id"].as_str().unwrap();
    let (status, _) = app
        .post(
            &format!("/v1/webhooks/{webhook_id}/deliveries/{delivery_id}/retry"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let delivered = delivery_where(&app, webhook_id, |d| d["status"] == "succeeded").await;
    assert_eq!(delivered["attempts"], 2);
    assert_eq!(receiver.requests().len(), 2);
}

#[tokio::test]
async fn blocks_private_destinations_by_default() {
    let app = TestApp::with_worker(Destinations::PublicOnly).await;
    let receiver = Receiver::start().await;
    let by_ip = create_webhook(&app, json!({"url": receiver.url})).await;
    let by_name_url = receiver.url.replace("127.0.0.1", "localhost");
    let by_name = create_webhook(&app, json!({"url": by_name_url})).await;

    app.post("/v1/companies", json!({"name": "Acme"})).await;
    for webhook in [by_ip, by_name] {
        let webhook_id = webhook["id"].as_str().unwrap();
        let delivery = delivery_where(&app, webhook_id, |d| d["attempts"] == 1).await;
        let error = delivery["last_error"].as_str().unwrap();
        assert!(error.contains("blocked"), "unexpected error: {error}");
    }
    assert!(receiver.requests().is_empty());
}

#[tokio::test]
async fn paused_webhook_catches_up_when_reenabled() {
    let app = TestApp::with_worker(Destinations::AllowPrivate).await;
    let receiver = Receiver::start().await;
    let webhook = create_webhook(&app, json!({"url": receiver.url})).await;
    let uri = format!("/v1/webhooks/{}", webhook["id"].as_str().unwrap());

    let (_, paused) = app.patch(&uri, json!({"enabled": false})).await;
    assert_eq!(paused["enabled"], false);
    app.post("/v1/companies", json!({"name": "One"})).await;
    app.post("/v1/companies", json!({"name": "Two"})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(receiver.requests().is_empty());

    app.patch(&uri, json!({"enabled": true})).await;
    let requests = eventually("catch-up deliveries", || async {
        let requests = receiver.requests();
        (requests.len() == 2).then_some(requests)
    })
    .await;
    let mut names: Vec<Value> = requests
        .iter()
        .map(|(_, body)| serde_json::from_slice::<Value>(body).unwrap()["data"]["name"].clone())
        .collect();
    names.sort_by_key(|name| name.to_string());
    assert_eq!(names, vec![json!("One"), json!("Two")]);
}

#[tokio::test]
async fn validates_webhook_input_and_scope() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post("/v1/webhooks", json!({"url": "ftp://example.com"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (status, _) = app
        .send(
            axum::http::Method::POST,
            "/v1/webhooks",
            Some(&app.read_token),
            Some(json!({"url": "https://example.com/hook"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
