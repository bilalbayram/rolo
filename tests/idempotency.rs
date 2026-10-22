mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use rolo::service::api_keys::{self, Scope};
use rolo::service::idempotency::{self, Attempt, Outcome};
use serde_json::json;

async fn contact_count(app: &TestApp) -> usize {
    let (_, page) = app.get("/v1/contacts").await;
    page["data"].as_array().unwrap().len()
}

#[tokio::test]
async fn repeats_replay_the_first_response() {
    let app = TestApp::new().await;
    let ada = json!({"first_name": "Ada", "phone": "0532 123 45 67"});

    let (status, replayed, first) = app
        .post_idempotent("/v1/contacts", &app.token, "lead-1", ada.clone())
        .await;
    assert_eq!((status, replayed), (StatusCode::CREATED, None));

    let (status, replayed, second) = app
        .post_idempotent("/v1/contacts", &app.token, "lead-1", ada.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replayed.as_deref(), Some("true"));
    assert_eq!(second, first);
    assert_eq!(contact_count(&app).await, 1);

    // Errors are stored and replayed too.
    let invalid = json!({"first_name": "Grace", "phone": "nope"});
    for expected_replay in [None, Some("true")] {
        let (status, replayed, body) = app
            .post_idempotent("/v1/contacts", &app.token, "lead-2", invalid.clone())
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(replayed.as_deref(), expected_replay);
        assert_eq!(body["error"]["code"], "invalid");
    }
}

#[tokio::test]
async fn keys_are_bound_to_one_request() {
    let app = TestApp::new().await;
    let (status, _, _) = app
        .post_idempotent(
            "/v1/contacts",
            &app.token,
            "k",
            json!({"first_name": "Ada"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    for (uri, body) in [
        ("/v1/contacts", json!({"first_name": "Grace"})),
        ("/v1/companies", json!({"first_name": "Ada"})),
    ] {
        let (status, replayed, body) = app.post_idempotent(uri, &app.token, "k", body).await;
        assert_eq!((status, replayed), (StatusCode::UNPROCESSABLE_ENTITY, None));
        assert_eq!(
            body["error"]["message"],
            "this Idempotency-Key was already used for a different request"
        );
    }

    let (status, _, body) = app
        .post_idempotent("/v1/contacts", &app.token, "", json!({"first_name": "Ada"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn keys_are_scoped_per_api_key() {
    let app = TestApp::new().await;
    let (_, other_token) = api_keys::create(app.db(), "other", Scope::Write, None)
        .await
        .unwrap();
    let ada = json!({"first_name": "Ada"});
    for token in [&app.token, &other_token] {
        let (status, replayed, _) = app
            .post_idempotent("/v1/contacts", token, "same-key", ada.clone())
            .await;
        assert_eq!((status, replayed), (StatusCode::CREATED, None));
    }
    assert_eq!(contact_count(&app).await, 2);
}

#[tokio::test]
async fn a_key_in_progress_conflicts() {
    let app = TestApp::new().await;
    let keys = api_keys::list(app.db()).await.unwrap();
    let key_id = &keys.iter().find(|key| key.name == "test").unwrap().id;
    let body = json!({"first_name": "Ada"});
    let body_hash = {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(body.to_string().as_bytes()))
    };
    let attempt = Attempt {
        api_key_id: key_id,
        key: "slow",
        request: "POST /v1/contacts",
        body_hash: &body_hash,
    };
    let outcome = idempotency::begin(app.db(), &attempt).await.unwrap();
    assert!(matches!(outcome, Outcome::Run));

    let (status, _, body) = app
        .post_idempotent("/v1/contacts", &app.token, "slow", body)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        "a request with this Idempotency-Key is still in progress"
    );
}

#[tokio::test]
async fn keys_expire_after_a_day() {
    let app = TestApp::new().await;
    let ada = json!({"first_name": "Ada"});
    app.post_idempotent("/v1/contacts", &app.token, "old", ada.clone())
        .await;
    let two_days_ago = chrono::Utc::now() - chrono::Duration::hours(48);
    sqlx::query("UPDATE idempotency_keys SET created_at = ?")
        .bind(two_days_ago)
        .execute(&app.db().writer)
        .await
        .unwrap();

    let (status, replayed, _) = app
        .post_idempotent("/v1/contacts", &app.token, "old", ada)
        .await;
    assert_eq!((status, replayed), (StatusCode::CREATED, None));
    assert_eq!(contact_count(&app).await, 2);
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys")
        .fetch_one(&app.db().reader)
        .await
        .unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn only_posts_are_idempotent() {
    let app = TestApp::new().await;
    let (_, contact) = app.post("/v1/contacts", json!({"first_name": "Ada"})).await;
    let uri = format!("/v1/contacts/{}", contact["id"].as_str().unwrap());
    // A PATCH isn't recorded, so its key can't be replayed or clash with a POST.
    for last_name in ["Lovelace", "Byron"] {
        let (status, replayed, updated) = app
            .send_idempotent(
                Method::PATCH,
                &uri,
                &app.token,
                "k",
                json!({"last_name": last_name}),
            )
            .await;
        assert_eq!((status, replayed), (StatusCode::OK, None));
        assert_eq!(updated["last_name"], last_name);
    }
    let (status, replayed, _) = app
        .post_idempotent("/v1/contacts", &app.token, "k", json!({"first_name": "G"}))
        .await;
    assert_eq!((status, replayed), (StatusCode::CREATED, None));
}
