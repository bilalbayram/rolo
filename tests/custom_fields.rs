mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

async fn define(app: &TestApp, body: Value) -> Value {
    let (status, definition) = app.post("/v1/custom-fields", body).await;
    assert_eq!(status, StatusCode::CREATED, "{definition}");
    definition
}

/// Contact fields: a required `tier` select, plus typed optional fields.
async fn define_contact_fields(app: &TestApp) -> Value {
    let tier = define(
        app,
        json!({"entity_type": "contact", "key": "tier", "label": "Tier", "field_type": "select",
               "options": ["gold", "silver"], "required": true}),
    )
    .await;
    for (key, field_type) in [("linkedin", "url"), ("nps", "number"), ("renewal", "date")] {
        define(
            app,
            json!({"entity_type": "contact", "key": key, "field_type": field_type}),
        )
        .await;
    }
    tier
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap()
}

#[tokio::test]
async fn validates_and_normalizes_values_on_create() {
    let app = TestApp::new().await;
    define_contact_fields(&app).await;

    let (status, body) = app.post("/v1/contacts", json!({"first_name": "Ada"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(message(&body), "custom.tier: required");

    // Every problem is reported at once.
    let custom = json!({"tier": "bronze", "nps": "9", "linkedin": "not a url"});
    let (status, body) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "custom": custom}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = message(&body);
    for problem in [
        "custom.tier: must be one of: gold, silver",
        "custom.nps: must be a number",
        "custom.linkedin: must be an http(s) URL",
    ] {
        assert!(message.contains(problem), "{message}");
    }

    let custom =
        json!({"tier": "gold", "nps": 9, "renewal": " 2027-01-31 ", "favourite_color": "teal"});
    let (status, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "custom": custom}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{contact}");
    assert_eq!(
        contact["custom"],
        json!({"tier": "gold", "nps": 9, "renewal": "2027-01-31", "favourite_color": "teal"}),
        "values are normalized; undefined keys stay free-form"
    );
}

#[tokio::test]
async fn validates_patches_and_protects_required_fields() {
    let app = TestApp::new().await;
    define_contact_fields(&app).await;
    let (_, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "custom": {"tier": "gold"}}),
        )
        .await;
    let uri = format!("/v1/contacts/{}", contact["id"].as_str().unwrap());

    let (status, body) = app.patch(&uri, json!({"custom": {"tier": null}})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        message(&body),
        "custom.tier: required and cannot be removed"
    );

    let (status, _) = app.patch(&uri, json!({"custom": {"nps": "high"}})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, updated) = app
        .patch(&uri, json!({"custom": {"tier": "silver", "nps": 7}}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["custom"], json!({"tier": "silver", "nps": 7}));

    // Definitions apply only to their record type.
    let (status, _) = app
        .post(
            "/v1/companies",
            json!({"name": "Acme", "custom": {"tier": "bronze"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn manages_definitions() {
    let app = TestApp::new().await;
    let tier = define_contact_fields(&app).await;
    let tier_uri = format!("/v1/custom-fields/{}", tier["id"].as_str().unwrap());

    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "contact", "key": "tier", "field_type": "text"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "deal", "key": "Bad Key", "field_type": "text"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "deal", "key": "plan", "field_type": "select"}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "select needs options"
    );

    let (_, fields) = app.get("/v1/custom-fields?entity_type=contact").await;
    let keys: Vec<&str> = fields
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["linkedin", "nps", "renewal", "tier"]);
    let (_, fields) = app.get("/v1/custom-fields?entity_type=deal").await;
    assert!(fields.as_array().unwrap().is_empty());

    let (_, updated) = app
        .patch(
            &tier_uri,
            json!({"options": ["gold", "silver", "bronze"], "required": false}),
        )
        .await;
    assert_eq!(updated["options"], json!(["gold", "silver", "bronze"]));
    let (status, _) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Bo", "custom": {"tier": "bronze"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // After deleting the definition, the key is free-form again.
    assert_eq!(app.delete(&tier_uri).await, StatusCode::NO_CONTENT);
    let (status, _) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Cy", "custom": {"tier": 3}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn agents_can_discover_fields_and_see_validation_errors() {
    let app = TestApp::new().await;
    define_contact_fields(&app).await;

    let fields = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "list_custom_fields", "arguments": {"entity_type": "contact"}}),
        )
        .await;
    let tier = &fields["structuredContent"]["custom_fields"][3];
    assert_eq!(tier["key"], "tier");
    assert_eq!(tier["options"], json!(["gold", "silver"]));

    let arguments = json!({"first_name": "Ada", "custom": {"tier": "platinum"}});
    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "create_contact", "arguments": arguments}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["content"][0]["text"],
        "invalid: custom.tier: must be one of: gold, silver"
    );
}
