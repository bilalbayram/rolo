mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

/// `(entity_type, title)` of each hit, in rank order.
async fn search(app: &TestApp, query: &str) -> Vec<(String, String)> {
    let (status, results) = app.get(&format!("/v1/search?{query}")).await;
    assert_eq!(status, StatusCode::OK, "{results}");
    results["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| {
            let field = |name: &str| hit[name].as_str().unwrap().to_string();
            (field("entity_type"), field("title"))
        })
        .collect()
}

fn ids(page: &Value) -> Vec<&str> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap())
        .collect()
}

/// A small CRM: Zoë Robotics with one contact, one deal and one note.
async fn seed(app: &TestApp) -> (Value, Value, Value, Value) {
    let (_, company) = app
        .post(
            "/v1/companies",
            json!({"name": "Zoë Robotics", "domain": "zoe.io", "custom": {"industry": "automation"}}),
        )
        .await;
    let (_, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "last_name": "Lovelace", "email": "ada@zoe.io",
                   "company_id": company["id"], "custom": {"linkedin": "adalovelace"}}),
        )
        .await;
    let (_, deal) = app
        .post(
            "/v1/deals",
            json!({"title": "Factory rollout", "company_id": company["id"]}),
        )
        .await;
    let (_, note) = app
        .post(
            "/v1/activities",
            json!({"kind": "note", "body": "Discussed the robotics budget for 2027",
                   "company_id": company["id"]}),
        )
        .await;
    (company, contact, deal, note)
}

#[tokio::test]
async fn ranks_matches_across_record_types() {
    let app = TestApp::new().await;
    seed(&app).await;

    // Title matches outrank body matches; accents are ignored; words match as prefixes.
    let hits = search(&app, "q=robot").await;
    assert_eq!(
        hits,
        vec![
            ("company".into(), "Zoë Robotics".into()),
            ("activity".into(), "note".into())
        ]
    );

    // Email and domain tokens, and custom field values, are searchable.
    let hits = search(&app, "q=zoe").await;
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(
        search(&app, "q=adalovelace").await,
        vec![("contact".into(), "Ada Lovelace".into())]
    );
    assert_eq!(
        search(&app, "q=automation").await,
        vec![("company".into(), "Zoë Robotics".into())]
    );

    // Every word must match.
    assert_eq!(
        search(&app, "q=robotics+2027").await,
        vec![("activity".into(), "note".into())]
    );

    // Filter by type.
    assert_eq!(
        search(&app, "q=zoe&types=contact").await,
        vec![("contact".into(), "Ada Lovelace".into())]
    );
    let (status, body) = app.get("/v1/search?q=zoe&types=people").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Punctuation is not parsed as FTS5 syntax.
    assert!(search(&app, "q=%22%29+OR+*").await.is_empty());
}

#[tokio::test]
async fn snippets_highlight_matches() {
    let app = TestApp::new().await;
    seed(&app).await;
    let (_, results) = app.get("/v1/search?q=budget").await;
    let snippet = results["data"][0]["snippet"].as_str().unwrap();
    assert!(snippet.contains("**budget**"), "{snippet}");
}

#[tokio::test]
async fn index_follows_updates_and_deletes() {
    let app = TestApp::new().await;
    let (company, _, deal, _) = seed(&app).await;
    let company_uri = format!("/v1/companies/{}", company["id"].as_str().unwrap());

    app.patch(&company_uri, json!({"name": "Acme"})).await;
    assert_eq!(
        search(&app, "q=acme").await,
        vec![("company".into(), "Acme".into())]
    );
    assert_eq!(search(&app, "q=robot&types=company").await, vec![]);

    app.delete(&format!("/v1/deals/{}", deal["id"].as_str().unwrap()))
        .await;
    assert!(search(&app, "q=factory").await.is_empty());
}

#[tokio::test]
async fn list_endpoints_filter_with_the_index() {
    let app = TestApp::new().await;
    let (company, contact, deal, note) = seed(&app).await;

    let (_, contacts) = app.get("/v1/contacts?q=zoe").await;
    assert_eq!(ids(&contacts), vec![contact["id"].as_str().unwrap()]);
    let (_, companies) = app.get("/v1/companies?q=automation").await;
    assert_eq!(ids(&companies), vec![company["id"].as_str().unwrap()]);
    let (_, deals) = app.get("/v1/deals?q=roll").await;
    assert_eq!(ids(&deals), vec![deal["id"].as_str().unwrap()]);
    let (_, activities) = app.get("/v1/activities?q=budget").await;
    assert_eq!(ids(&activities), vec![note["id"].as_str().unwrap()]);

    // A query with no searchable words matches nothing rather than everything.
    let (status, contacts) = app.get("/v1/contacts?q=%40%40").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids(&contacts).is_empty());
}

#[tokio::test]
async fn mcp_search_tool() {
    let app = TestApp::new().await;
    let (_, contact, _, _) = seed(&app).await;
    let result = app
        .mcp(
            &app.read_token,
            "tools/call",
            json!({"name": "search", "arguments": {"query": "lovelace", "types": ["contact"]}}),
        )
        .await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["data"][0]["id"], contact["id"]);
}

#[tokio::test]
async fn turkish_dotless_and_dotted_i_match_plain_i() {
    let app = TestApp::new().await;
    let (_, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ayşe", "last_name": "Işık", "custom": {"city": "İzmir"}}),
        )
        .await;
    let hits = [("contact".to_string(), "Ayşe Işık".to_string())];

    for q in ["isik", "Işık", "IŞIK", "ışık", "ayse+isi", "izmir", "İZMİR"] {
        assert_eq!(search(&app, &format!("q={q}")).await, hits, "{q}");
    }
    let (_, results) = app.get("/v1/search?q=isik").await;
    assert_eq!(results["data"][0]["snippet"], "Ayşe **Işık**");
    let (_, page) = app.get("/v1/contacts?q=IŞIK").await;
    assert_eq!(ids(&page), [contact["id"].as_str().unwrap()]);

    // Updates are folded too, and old words leave the index.
    let (status, _) = app
        .patch(
            &format!("/v1/contacts/{}", contact["id"].as_str().unwrap()),
            json!({"last_name": "Yıldız"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search(&app, "q=yildiz").await.len(), 1);
    assert!(search(&app, "q=isik").await.is_empty());
}

#[tokio::test]
async fn deal_search_also_matches_the_primary_contact() {
    let app = TestApp::new().await;
    let (_, ayse) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ayşe", "last_name": "Yılmaz", "email": "ayse@example.com",
                   "phone": "+905321234567"}),
        )
        .await;
    let (_, mehmet) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Mehmet", "phone": "+905339876543"}),
        )
        .await;
    let mut deals = Vec::new();
    for (title, contact) in [
        ("Düğün haziran", &ayse),
        ("Şirket yemeği", &mehmet),
        ("Ayşe hanım nişan", &json!({"id": null})),
    ] {
        let (status, deal) = app
            .post(
                "/v1/deals",
                json!({"title": title, "contact_id": contact["id"]}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{deal}");
        deals.push(deal["id"].as_str().unwrap().to_string());
    }

    for (q, expected) in [
        // The contact's name, or the deal's own title.
        ("ayse", vec![&deals[0], &deals[2]]),
        ("yilmaz", vec![&deals[0]]),
        ("ayse@example", vec![&deals[0]]),
        // A phone number in local or international format.
        ("0532 123 45 67", vec![&deals[0]]),
        ("05321234567", vec![&deals[0]]),
        ("%2B90 533 987 65 43", vec![&deals[1]]),
        ("0532 000 00 00", vec![]),
        ("yemegi", vec![&deals[1]]),
    ] {
        let (status, page) = app
            .get(&format!(
                "/v1/deals?q={}&include_total=true",
                q.replace(' ', "%20")
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{q}: {page}");
        assert_eq!(ids(&page), expected, "{q}");
        assert_eq!(page["total"], expected.len(), "{q}");
    }

    // Contact search takes local phone numbers too.
    let (_, page) = app.get("/v1/contacts?q=0532%20123%2045%2067").await;
    assert_eq!(ids(&page), [ayse["id"].as_str().unwrap()]);
}
