mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

/// Creates deals expected to close on these dates; returns their IDs in order.
async fn deals_closing(app: &TestApp, dates: &[Option<&str>]) -> Vec<String> {
    let mut ids = Vec::new();
    for (i, date) in dates.iter().enumerate() {
        let (status, deal) = app
            .post(
                "/v1/deals",
                json!({"title": format!("Deal {i}"), "expected_close_date": date}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{deal}");
        ids.push(deal["id"].as_str().unwrap().to_string());
    }
    ids
}

/// The IDs in `d` at `indices`.
fn pick(d: &[String], indices: &[usize]) -> Vec<String> {
    indices.iter().map(|&i| d[i].clone()).collect()
}

fn ids(page: &Value) -> Vec<String> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|deal| deal["id"].as_str().unwrap().to_string())
        .collect()
}

/// Every deal for `query`, fetched two at a time.
async fn all_pages(app: &TestApp, query: &str) -> Vec<String> {
    app.all_ids(&format!("/v1/deals?{query}")).await
}

#[tokio::test]
async fn filters_and_sorts_by_expected_close_date() {
    let app = TestApp::new().await;
    let d = deals_closing(
        &app,
        &[
            Some("2026-11-10"),
            Some("2026-11-01"),
            None,
            Some("2026-11-01"),
            Some("2026-12-05"),
        ],
    )
    .await;

    // Inclusive range; deals without a date are left out. Default order is by ID.
    let (status, page) = app
        .get("/v1/deals?expected_close_from=2026-11-01&expected_close_to=2026-11-10")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&page), pick(&d, &[0, 1, 3]));

    assert_eq!(all_pages(&app, "").await, d);
    let mut newest_first = d.clone();
    newest_first.reverse();
    assert_eq!(all_pages(&app, "sort=-id").await, newest_first);
    assert_eq!(
        all_pages(&app, "sort=expected_close_date").await,
        pick(&d, &[1, 3, 0, 4, 2])
    );
    assert_eq!(
        all_pages(&app, "sort=-expected_close_date").await,
        pick(&d, &[4, 0, 3, 1, 2])
    );
    assert_eq!(
        all_pages(
            &app,
            "sort=expected_close_date&expected_close_to=2026-11-30"
        )
        .await,
        pick(&d, &[1, 3, 0])
    );

    let (_, page) = app.get("/v1/deals?sort=expected_close_date&limit=1").await;
    assert_eq!(page["next_cursor"], format!("2026-11-01/{}", d[1]));
    for query in [
        "sort=expected_close_date&cursor=garbage",
        "sort=expected_close_date&cursor=2026-13-01/deal_x",
        "sort=closed_at",
        "expected_close_from=tomorrow",
    ] {
        let (status, _) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
    }

    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "search_deals", "arguments": {
                "expected_close_from": "2026-11-05", "sort": "-expected_close_date"}}),
        )
        .await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(ids(&result["structuredContent"]), pick(&d, &[4, 0]));
}

#[tokio::test]
async fn upsert_creates_then_updates_the_matching_deal() {
    let app = TestApp::new().await;
    for definition in [
        json!({"entity_type": "deal", "key": "conversation_id", "field_type": "text", "unique": true}),
        json!({"entity_type": "deal", "key": "mihrabat_inquiry_id", "field_type": "number", "unique": true}),
        json!({"entity_type": "deal", "key": "intent", "field_type": "text"}),
    ] {
        let (status, body) = app.post("/v1/custom-fields", definition).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let by_conversation = json!(["custom.conversation_id"]);

    let (status, first) = app
        .post(
            "/v1/deals/upsert",
            json!({"title": "Düğün · Ayşe", "custom": {"conversation_id": "c1", "intent": "wedding"},
                   "match_on": by_conversation}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["created"], true);
    assert_eq!(first["deal"]["stage_id"], "stg_lead");
    let id = first["deal"]["id"].clone();

    // Only the given fields change; custom merges; stage moves follow PATCH rules.
    let (status, second) = app
        .post(
            "/v1/deals/upsert",
            json!({"stage_id": "stg_won", "amount": 500000, "title": " ",
                   "custom": {"conversation_id": "c1", "guests": "120"},
                   "match_on": by_conversation}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let deal = &second["deal"];
    assert_eq!((&second["created"], &deal["id"]), (&json!(false), &id));
    assert_eq!(deal["title"], "Düğün · Ayşe");
    assert_eq!(deal["status"], "won");
    assert!(deal["closed_at"].is_string());
    assert_eq!(
        deal["custom"],
        json!({"conversation_id": "c1", "intent": "wedding", "guests": "120"})
    );
    let (_, events) = app.get("/v1/events?type=deal.stage_changed").await;
    assert_eq!(events["data"][0]["entity_id"], id);

    // Keys are tried in order; a later key can match and fill in an earlier one.
    let both = json!(["custom.mihrabat_inquiry_id", "custom.conversation_id"]);
    let (status, third) = app
        .post(
            "/v1/deals/upsert",
            json!({"custom": {"mihrabat_inquiry_id": 7, "conversation_id": "c1"}, "match_on": both}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(third["deal"]["id"], id);
    let (status, fourth) = app
        .post(
            "/v1/deals/upsert",
            json!({"custom": {"mihrabat_inquiry_id": 7.0}, "match_on": both}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fourth["deal"]["id"], id);

    let (_, page) = app.get("/v1/deals").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 1);

    for (body, problem) in [
        (
            json!({"custom": {"conversation_id": "c2"}, "match_on": by_conversation}),
            "title is required to create a deal",
        ),
        (
            json!({"title": "X", "custom": {"intent": "x"}, "match_on": ["custom.intent"]}),
            "cannot match on custom.intent: it is not a unique custom field for deals",
        ),
        (
            json!({"title": "X", "match_on": ["contact_id"]}),
            "cannot match deals on \"contact_id\"",
        ),
        (
            json!({"title": "X", "match_on": []}),
            "match_on must list at least one key",
        ),
        (json!({"title": "X"}), "missing field `match_on`"),
    ] {
        let (status, error) = app.post("/v1/deals/upsert", body.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains(problem), "{message}");
    }
}
