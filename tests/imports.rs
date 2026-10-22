mod common;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use common::TestApp;
use serde_json::{Value, json};

const MARCH: &str = "2024-03-01T13:00:00+03:00";

fn time(value: &Value) -> DateTime<Utc> {
    value.as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn records_can_keep_their_original_creation_time() {
    let app = TestApp::new().await;
    let march: DateTime<Utc> = MARCH.parse().unwrap();

    let (_, later) = app
        .post("/v1/contacts", json!({"first_name": "Later"}))
        .await;
    let (status, imported) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Imported", "created_at": MARCH}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{imported}");
    assert_eq!(time(&imported["created_at"]), march);
    assert_eq!(time(&imported["updated_at"]), march);
    // The ID encodes the original time, so it sorts before newer records.
    let ids = app.all_ids("/v1/contacts?sort=id").await;
    assert_eq!(
        ids,
        [&imported["id"], &later["id"]].map(|id| id.as_str().unwrap())
    );

    // Events still record when the change really happened.
    let (_, events) = app.get("/v1/events?type=contact.created").await;
    let event_times: Vec<DateTime<Utc>> = events["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| time(&event["occurred_at"]))
        .collect();
    assert!(event_times.iter().all(|at| *at > march), "{events}");

    let (status, company) = app
        .post(
            "/v1/companies",
            json!({"name": "Acme", "created_at": MARCH}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(time(&company["created_at"]), march);

    let (status, deal) = app
        .post(
            "/v1/deals",
            json!({"title": "Old win", "stage_id": "stg_won", "created_at": MARCH}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(time(&deal["created_at"]), march);
    assert_eq!(time(&deal["closed_at"]), march);

    let (status, note) = app
        .post(
            "/v1/activities",
            json!({"kind": "note", "body": "Called", "deal_id": deal["id"], "created_at": MARCH}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{note}");
    assert_eq!(time(&note["created_at"]), march);
    assert_eq!(time(&note["occurred_at"]), march);

    let tomorrow = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    for (path, body) in [
        (
            "/v1/contacts",
            json!({"first_name": "Soon", "created_at": tomorrow}),
        ),
        (
            "/v1/companies",
            json!({"name": "Soon", "created_at": tomorrow}),
        ),
        (
            "/v1/deals",
            json!({"title": "Soon", "created_at": tomorrow}),
        ),
        (
            "/v1/contacts",
            json!({"first_name": "Bad", "created_at": "2024-03-01"}),
        ),
    ] {
        let (status, _) = app.post(path, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
}

#[tokio::test]
async fn upserts_use_created_at_only_when_creating() {
    let app = TestApp::new().await;
    let march: DateTime<Utc> = MARCH.parse().unwrap();
    let body = json!({"first_name": "Ayşe", "phone": "05321234567", "created_at": MARCH});
    let (status, created) = app.post("/v1/contacts/upsert", body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(time(&created["contact"]["created_at"]), march);
    let (status, updated) = app.post("/v1/contacts/upsert", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(time(&updated["contact"]["created_at"]), march);
    assert!(time(&updated["contact"]["updated_at"]) > march);

    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "deal", "key": "mihrabat_inquiry_id", "field_type": "number", "unique": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, deal) = app
        .post(
            "/v1/deals/upsert",
            json!({"title": "Düğün", "custom": {"mihrabat_inquiry_id": 52}, "created_at": MARCH,
                   "match_on": ["custom.mihrabat_inquiry_id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{deal}");
    assert_eq!(time(&deal["deal"]["created_at"]), march);
}
