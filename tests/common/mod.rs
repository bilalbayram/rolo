//! Shared harness for integration tests: an app on a temporary database,
//! driven in-process with `tower::ServiceExt::oneshot`.

// Each test binary uses a different subset of the harness.
#![allow(dead_code)]

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use rolo::AppConfig;
use rolo::db::Db;
use rolo::delivery::{self, RetryPolicy, WorkerConfig};
use rolo::phone::Region;
use rolo::service::api_keys::{self, Scope};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Which webhook destinations the worker may deliver to.
pub enum Destinations {
    PublicOnly,
    AllowPrivate,
}

pub struct TestApp {
    router: Router,
    db: Db,
    pub token: String,
    pub read_token: String,
    path: PathBuf,
    shutdown: CancellationToken,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

impl TestApp {
    /// App without the webhook worker.
    pub async fn new() -> Self {
        Self::start(None).await
    }

    /// App with the webhook worker polling every 20ms.
    pub async fn with_worker(destinations: Destinations) -> Self {
        Self::with_retry_policy(destinations, RetryPolicy::default()).await
    }

    /// App with the webhook worker polling every 20ms, using `retry`.
    pub async fn with_retry_policy(destinations: Destinations, retry: RetryPolicy) -> Self {
        let config = WorkerConfig {
            allow_private_destinations: matches!(destinations, Destinations::AllowPrivate),
            poll_interval: std::time::Duration::from_millis(20),
            retry,
        };
        Self::start(Some(config)).await
    }

    async fn start(worker: Option<WorkerConfig>) -> Self {
        let path = std::env::temp_dir().join(format!("rolo-test-{}.db", ulid::Ulid::generate()));
        let db = Db::open(&path).await.unwrap();
        let shutdown = CancellationToken::new();
        if let Some(config) = worker {
            delivery::spawn(db.clone(), config, shutdown.clone()).unwrap();
        }
        let (_, token) = api_keys::create(&db, "test", Scope::Write, None)
            .await
            .unwrap();
        let (_, read_token) = api_keys::create(&db, "test-ro", Scope::Read, None)
            .await
            .unwrap();
        let router = rolo::app(
            db.clone(),
            AppConfig {
                allowed_hosts: vec![],
                default_region: Region::TURKEY,
            },
        );
        Self {
            router,
            db,
            token,
            read_token,
            path,
            shutdown,
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// A write-scoped API key that acts as `user_id`.
    pub async fn key_for_user(&self, user_id: &str) -> String {
        let (_, token) = api_keys::create(&self.db, "user key", Scope::Write, Some(user_id))
            .await
            .unwrap();
        token
    }

    pub async fn send(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "localhost");
        if let Some(token) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let req = match body {
            Some(body) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, value)
    }

    /// POST with an `Idempotency-Key`. Returns the `Idempotent-Replayed` header too.
    pub async fn post_idempotent(
        &self,
        uri: &str,
        token: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Option<String>, Value) {
        self.send_idempotent(Method::POST, uri, token, key, body)
            .await
    }

    /// A request with an `Idempotency-Key`. Returns the `Idempotent-Replayed`
    /// header too.
    pub async fn send_idempotent(
        &self,
        method: Method,
        uri: &str,
        token: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Option<String>, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let replayed = res
            .headers()
            .get("idempotent-replayed")
            .map(|value| value.to_str().unwrap().to_string());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, replayed, serde_json::from_slice(&bytes).unwrap())
    }

    /// IDs of every record listed at `uri` (which must have a query string),
    /// fetched two at a time by following `next_cursor`.
    pub async fn all_ids(&self, uri: &str) -> Vec<String> {
        let mut ids = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut page_uri = format!("{uri}&limit=2");
            if let Some(cursor) = &cursor {
                page_uri.push_str(&format!("&cursor={cursor}"));
            }
            let (status, page) = self.get(&page_uri).await;
            assert_eq!(status, StatusCode::OK, "{page_uri}: {page}");
            for record in page["data"].as_array().unwrap() {
                ids.push(record["id"].as_str().unwrap().to_string());
            }
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_string()),
                None => return ids,
            }
        }
    }

    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send(Method::GET, uri, Some(&self.token), None).await
    }

    pub async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, uri, Some(&self.token), Some(body))
            .await
    }

    pub async fn patch(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::PATCH, uri, Some(&self.token), Some(body))
            .await
    }

    pub async fn delete(&self, uri: &str) -> StatusCode {
        self.send(Method::DELETE, uri, Some(&self.token), None)
            .await
            .0
    }

    pub async fn mcp(&self, token: &str, method: &str, params: Value) -> Value {
        let req = Request::post("/mcp")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-06-18")
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(),
            ))
            .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        value["result"].clone()
    }
}
