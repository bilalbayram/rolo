mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};

#[tokio::test]
async fn requires_valid_key_and_enforces_scope() {
    let app = TestApp::new().await;

    let (status, body) = app.send(Method::GET, "/v1/contacts", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    let (status, _) = app
        .send(Method::GET, "/v1/contacts", Some("rolo_nope"), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = app
        .send(Method::GET, "/v1/contacts", Some(&app.read_token), None)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app
        .send(
            Method::POST,
            "/v1/companies",
            Some(&app.read_token),
            Some(json!({"name": "Acme"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "forbidden");

    let (status, body) = app.send(Method::GET, "/health", None, None).await;
    assert_eq!((status, body), (StatusCode::OK, json!("ok")));
}

#[tokio::test]
async fn serves_docs_with_a_favicon() {
    let app = TestApp::new().await;
    let (status, html) = app.send(Method::GET, "/docs", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let html = html.as_str().unwrap();
    assert!(html.contains(r#"<link rel="icon" href="/favicon.svg""#));
    assert!(html.contains(r#""title":"rolo""#), "the spec is embedded");

    let (status, svg) = app.send(Method::GET, "/favicon.svg", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(svg.as_str().unwrap().starts_with("<svg"));
    let (status, _) = app.send(Method::GET, "/favicon.ico", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn contact_crud_validation_and_patch_semantics() {
    let app = TestApp::new().await;

    let (status, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "email": " Ada@Example.com ", "phone": "+44 20 7946 0958", "custom": {"tier": "gold", "nps": 9}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(contact["email"], "ada@example.com");
    let id = contact["id"].as_str().unwrap();
    assert!(id.starts_with("con_"));

    let (status, body) = app
        .post("/v1/contacts", json!({"email": "ADA@example.com"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        "a contact with this email already exists"
    );

    let (status, _) = app.post("/v1/contacts", json!({"phone": "123"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Omitted fields stay, null clears, custom merges and null removes keys.
    let (status, updated) = app
        .patch(
            &format!("/v1/contacts/{id}"),
            json!({"last_name": "Lovelace", "phone": null, "custom": {"nps": null, "owner": "bilal"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["first_name"], "Ada");
    assert_eq!(updated["last_name"], "Lovelace");
    assert_eq!(updated["phone"], Value::Null);
    assert_eq!(updated["custom"], json!({"tier": "gold", "owner": "bilal"}));

    let (_, page) = app.get("/v1/contacts?q=lovel").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 1);

    assert_eq!(
        app.delete(&format!("/v1/contacts/{id}")).await,
        StatusCode::NO_CONTENT
    );
    let (status, body) = app.get(&format!("/v1/contacts/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn deal_stage_changes_set_closed_at_and_emit_events() {
    let app = TestApp::new().await;

    let (status, deal) = app
        .post(
            "/v1/deals",
            json!({"title": "Rollout", "amount": 1_250_000, "currency": "eur"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(deal["stage_id"], "stg_lead");
    assert_eq!(deal["pipeline_id"], "pip_sales");
    assert_eq!(deal["status"], "open");
    assert_eq!(deal["currency"], "EUR");
    let id = deal["id"].as_str().unwrap();

    let (_, won) = app
        .patch(&format!("/v1/deals/{id}"), json!({"stage_id": "stg_won"}))
        .await;
    assert_eq!(won["status"], "won");
    assert!(won["closed_at"].is_string());

    let (_, won_deals) = app.get("/v1/deals?status=won").await;
    assert_eq!(won_deals["data"].as_array().unwrap().len(), 1);

    let (_, reopened) = app
        .patch(
            &format!("/v1/deals/{id}"),
            json!({"stage_id": "stg_qualified"}),
        )
        .await;
    assert_eq!(reopened["status"], "open");
    assert_eq!(reopened["closed_at"], Value::Null);

    let (status, _) = app
        .patch(
            &format!("/v1/deals/{id}"),
            json!({"stage_id": "stg_missing"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, events) = app.get("/v1/events?type=deal.stage_changed").await;
    let changes: Vec<_> = events["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["data"]["from_stage_id"].clone(),
                e["data"]["to_stage_id"].clone(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        vec![
            (json!("stg_lead"), json!("stg_won")),
            (json!("stg_won"), json!("stg_qualified"))
        ]
    );

    // `after` resumes the feed.
    let next = events["next_after"].as_i64().unwrap();
    let (_, rest) = app.get(&format!("/v1/events?after={next}")).await;
    assert!(rest["data"].as_array().unwrap().is_empty());
    assert_eq!(rest["next_after"], next);
}

#[tokio::test]
async fn cursor_pagination_walks_all_pages() {
    let app = TestApp::new().await;
    for i in 0..5 {
        app.post("/v1/companies", json!({"name": format!("Company {i}")}))
            .await;
    }
    let mut names = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/v1/companies?limit=2&cursor={c}"),
            None => "/v1/companies?limit=2".to_string(),
        };
        let (_, page) = app.get(&uri).await;
        names.extend(
            page["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].clone()),
        );
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(
        names,
        (0..5)
            .map(|i| json!(format!("Company {i}")))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn activities_require_a_link_and_survive_record_deletion() {
    let app = TestApp::new().await;
    let (_, contact) = app
        .post("/v1/contacts", json!({"first_name": "Grace"}))
        .await;
    let (_, deal) = app.post("/v1/deals", json!({"title": "Pilot"})).await;

    let (status, _) = app
        .post("/v1/activities", json!({"kind": "note", "body": "orphan"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, task) = app
        .post(
            "/v1/activities",
            json!({"kind": "task", "subject": "Send contract", "due_at": "2026-11-01T09:00:00Z",
                   "contact_id": contact["id"], "deal_id": deal["id"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let task_id = task["id"].as_str().unwrap();

    let (_, open) = app.get("/v1/activities?open=true").await;
    assert_eq!(open["data"].as_array().unwrap().len(), 1);
    app.patch(
        &format!("/v1/activities/{task_id}"),
        json!({"completed_at": "2026-10-23T10:00:00Z"}),
    )
    .await;
    let (_, open) = app.get("/v1/activities?open=true").await;
    assert!(open["data"].as_array().unwrap().is_empty());

    app.delete(&format!("/v1/contacts/{}", contact["id"].as_str().unwrap()))
        .await;
    let (status, task) = app.get(&format!("/v1/activities/{task_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(task["contact_id"], Value::Null);
    assert_eq!(task["deal_id"], deal["id"]);
}

#[tokio::test]
async fn bad_requests_use_the_error_envelope() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post("/v1/companies", json!({"name": "Acme", "nmae": "typo"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown field `nmae`")
    );

    let (status, body) = app.get("/v1/companies?limit=lots").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid");
}

#[tokio::test]
async fn mcp_tools_share_data_and_respect_scope() {
    let app = TestApp::new().await;

    let tools = app.mcp(&app.read_token, "tools/list", json!({})).await;
    let names: Vec<_> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"search_contacts") && names.contains(&"update_deal"));

    let denied = app
        .mcp(
            &app.read_token,
            "tools/call",
            json!({"name": "create_company", "arguments": {"name": "Acme"}}),
        )
        .await;
    assert_eq!(denied["isError"], true);

    let created = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "create_company", "arguments": {"name": "Acme", "domain": "acme.com"}}),
        )
        .await;
    assert_ne!(created["isError"], true);
    let company_id = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Visible over REST immediately.
    let (status, company) = app.get(&format!("/v1/companies/{company_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(company["domain"], "acme.com");

    let missing = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "get_deal", "arguments": {"id": "deal_missing"}}),
        )
        .await;
    assert_eq!(missing["isError"], true);
    assert_eq!(missing["content"][0]["text"], "not_found: deal not found");
}

#[tokio::test]
async fn unknown_query_parameters_are_rejected() {
    let app = TestApp::new().await;
    for endpoint in [
        "/v1/contacts?",
        "/v1/companies?",
        "/v1/deals?",
        "/v1/activities?",
        "/v1/users?",
        "/v1/custom-fields?",
        "/v1/events?",
        "/v1/search?q=ada&",
    ] {
        let (status, body) = app.get(&format!("{endpoint}ownr_id=me")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{endpoint}");
        assert_eq!(
            body["error"]["message"], "unknown query parameter \"ownr_id\"",
            "{endpoint}"
        );
    }
    let (status, body) = app.get("/v1/contacts?limit=ten").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid query string:")
    );
    let (status, _) = app.get("/v1/contacts?limit=10&q=").await;
    assert_eq!(status, StatusCode::OK);

    // MCP tools are as strict.
    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "search_contacts", "arguments": {"ownr_id": "me"}}),
        )
        .await;
    assert_eq!(result["isError"], true, "{result}");
}

#[tokio::test]
async fn lists_sort_oldest_or_newest_first() {
    let app = TestApp::new().await;
    let mut contacts = Vec::new();
    let mut companies = Vec::new();
    for i in 0..5 {
        let (_, contact) = app
            .post("/v1/contacts", json!({"first_name": format!("C{i}")}))
            .await;
        contacts.push(contact["id"].as_str().unwrap().to_string());
        let (_, company) = app
            .post("/v1/companies", json!({"name": format!("Co{i}")}))
            .await;
        companies.push(company["id"].as_str().unwrap().to_string());
    }
    for (path, mut ids) in [("/v1/contacts", contacts), ("/v1/companies", companies)] {
        assert_eq!(app.all_ids(&format!("{path}?sort=id")).await, ids, "{path}");
        ids.reverse();
        assert_eq!(
            app.all_ids(&format!("{path}?sort=-id")).await,
            ids,
            "{path}"
        );
        let (status, _) = app.get(&format!("{path}?sort=name")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn lists_fetch_records_by_id() {
    let app = TestApp::new().await;
    let mut created: Vec<(&str, Vec<String>)> = Vec::new();
    for (path, body) in [
        ("/v1/contacts", json!({"first_name": "Ada"})),
        ("/v1/companies", json!({"name": "Acme"})),
        ("/v1/deals", json!({"title": "Rollout"})),
    ] {
        let mut ids = Vec::new();
        for _ in 0..3 {
            let (status, record) = app.post(path, body.clone()).await;
            assert_eq!(status, StatusCode::CREATED);
            ids.push(record["id"].as_str().unwrap().to_string());
        }
        created.push((path, ids));
    }

    for (path, ids) in &created {
        // The order given; repeats, blanks and unknown IDs are dropped.
        let query = format!("{},%20missing,{},,{}", ids[2], ids[0], ids[2]);
        let (status, page) = app.get(&format!("{path}?ids={query}")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let found: Vec<&str> = page["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["id"].as_str().unwrap())
            .collect();
        assert_eq!(found, [ids[2].as_str(), ids[0].as_str()], "{path}");
        assert_eq!(page["next_cursor"], Value::Null);

        for extra in ["&limit=1", "&sort=-id", "&cursor=x"] {
            let (status, body) = app.get(&format!("{path}?ids={}{extra}", ids[0])).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}{extra}");
            assert_eq!(
                body["error"]["message"],
                "ids can't be combined with limit, cursor or sort"
            );
        }
        let too_many: Vec<String> = (0..101).map(|i| format!("x{i}")).collect();
        let (status, _) = app.get(&format!("{path}?ids={}", too_many.join(","))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = app.get(&format!("{path}?ids=,")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Other filters still apply.
    let (_, page) = app
        .get(&format!("/v1/deals?ids={}&status=won", created[2].1[0]))
        .await;
    assert_eq!(page["data"], json!([]));
}
