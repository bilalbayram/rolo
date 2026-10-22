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
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut uri = format!("/v1/deals?limit=2&{query}");
        if let Some(cursor) = &cursor {
            uri.push_str(&format!("&cursor={cursor}"));
        }
        let (status, page) = app.get(&uri).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        all.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => return all,
        }
    }
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
