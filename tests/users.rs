mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};

async fn create_user(app: &TestApp, name: &str, email: &str) -> String {
    let (status, user) = app
        .post("/v1/users", json!({"name": name, "email": email}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    user["id"].as_str().unwrap().to_string()
}

async fn send_as(
    app: &TestApp,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    app.send(method, uri, Some(token), body).await
}

fn ids(page: &Value) -> Vec<String> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn new_records_default_to_the_keys_user() {
    let app = TestApp::new().await;
    let ada = create_user(&app, "Ada", "Ada@Example.com").await;
    let ada_key = app.key_for_user(&ada).await;

    let (_, me) = send_as(&app, &ada_key, Method::GET, "/v1/users/me", None).await;
    assert_eq!(me["id"], ada.as_str());
    assert_eq!(me["email"], "ada@example.com");
    let (status, _) = app.get("/v1/users/me").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the default test key has no user"
    );

    let body = json!({"name": "Acme"});
    let (_, owned) = send_as(&app, &ada_key, Method::POST, "/v1/companies", Some(body)).await;
    assert_eq!(owned["owner_id"], ada.as_str());

    let body = json!({"name": "Unowned", "owner_id": null});
    let (_, unowned) = send_as(&app, &ada_key, Method::POST, "/v1/companies", Some(body)).await;
    assert_eq!(unowned["owner_id"], Value::Null);

    let (_, keyless) = app.post("/v1/companies", json!({"name": "Beta"})).await;
    assert_eq!(keyless["owner_id"], Value::Null);
    let (status, body) = app
        .post("/v1/companies", json!({"name": "Gamma", "owner_id": "me"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("\"me\"")
    );

    let (status, body) = app
        .post(
            "/v1/users",
            json!({"name": "Ada 2", "email": "ADA@example.com"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn owner_filters_find_my_tasks_and_unassigned_work() {
    let app = TestApp::new().await;
    let ada = create_user(&app, "Ada", "ada@example.com").await;
    let bob = create_user(&app, "Bob", "bob@example.com").await;
    let ada_key = app.key_for_user(&ada).await;
    let (_, deal) = app.post("/v1/deals", json!({"title": "Pilot"})).await;

    let task = |owner: Value| json!({"kind": "task", "subject": "Follow up", "deal_id": deal["id"], "owner_id": owner});
    let (_, mine) = send_as(
        &app,
        &ada_key,
        Method::POST,
        "/v1/activities",
        Some(task(json!("me"))),
    )
    .await;
    let (_, bobs) = app.post("/v1/activities", task(json!(bob))).await;
    let (_, nobodys) = app.post("/v1/activities", task(Value::Null)).await;

    let (_, page) = send_as(
        &app,
        &ada_key,
        Method::GET,
        "/v1/activities?owner_id=me&open=true",
        None,
    )
    .await;
    assert_eq!(ids(&page), vec![mine["id"].as_str().unwrap()]);
    let (_, page) = app.get(&format!("/v1/activities?owner_id={bob}")).await;
    assert_eq!(ids(&page), vec![bobs["id"].as_str().unwrap()]);
    let (_, page) = app.get("/v1/activities?owner_id=none").await;
    assert_eq!(ids(&page), vec![nobodys["id"].as_str().unwrap()]);

    // Reassign via PATCH.
    let uri = format!("/v1/activities/{}", nobodys["id"].as_str().unwrap());
    let (_, reassigned) = send_as(
        &app,
        &ada_key,
        Method::PATCH,
        &uri,
        Some(json!({"owner_id": "me"})),
    )
    .await;
    assert_eq!(reassigned["owner_id"], ada.as_str());
}

#[tokio::test]
async fn deactivated_users_cannot_be_assigned_or_authenticate() {
    let app = TestApp::new().await;
    let bob = create_user(&app, "Bob", "bob@example.com").await;
    let bob_key = app.key_for_user(&bob).await;
    let (_, contact) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Grace", "owner_id": bob}),
        )
        .await;
    let contact_uri = format!("/v1/contacts/{}", contact["id"].as_str().unwrap());

    let (status, _) = app
        .patch(&contact_uri, json!({"owner_id": "usr_missing"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, deactivated) = app
        .patch(&format!("/v1/users/{bob}"), json!({"deactivated": true}))
        .await;
    assert!(deactivated["deactivated_at"].is_string());
    let (status, body) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Alan", "owner_id": bob}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _) = send_as(&app, &bob_key, Method::GET, "/v1/contacts", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Existing ownership is kept; deactivated users are hidden by default.
    let (_, contact) = app.get(&contact_uri).await;
    assert_eq!(contact["owner_id"], bob.as_str());
    let (_, users) = app.get("/v1/users").await;
    assert!(users.as_array().unwrap().is_empty());
    let (_, users) = app.get("/v1/users?include_deactivated=true").await;
    assert_eq!(users.as_array().unwrap().len(), 1);

    app.patch(&format!("/v1/users/{bob}"), json!({"deactivated": false}))
        .await;
    let (status, _) = send_as(&app, &bob_key, Method::GET, "/v1/contacts", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn deleting_a_user_unowns_records_and_revokes_keys() {
    let app = TestApp::new().await;
    let ada = create_user(&app, "Ada", "ada@example.com").await;
    let ada_key = app.key_for_user(&ada).await;
    let body = json!({"title": "Pilot"});
    let (_, deal) = send_as(&app, &ada_key, Method::POST, "/v1/deals", Some(body)).await;
    assert_eq!(deal["owner_id"], ada.as_str());

    assert_eq!(
        app.delete(&format!("/v1/users/{ada}")).await,
        StatusCode::NO_CONTENT
    );
    let (_, deal) = app
        .get(&format!("/v1/deals/{}", deal["id"].as_str().unwrap()))
        .await;
    assert_eq!(deal["owner_id"], Value::Null);
    let (status, _) = send_as(&app, &ada_key, Method::GET, "/v1/deals", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_tools_know_who_me_is() {
    let app = TestApp::new().await;
    let ada = create_user(&app, "Ada", "ada@example.com").await;
    let ada_key = app.key_for_user(&ada).await;

    let users = app
        .mcp(
            &ada_key,
            "tools/call",
            json!({"name": "list_users", "arguments": {}}),
        )
        .await;
    assert_eq!(users["structuredContent"]["me"], ada.as_str());
    assert_eq!(users["structuredContent"]["users"][0]["name"], "Ada");

    let arguments = json!({"first_name": "Grace"});
    let created = app
        .mcp(
            &ada_key,
            "tools/call",
            json!({"name": "create_contact", "arguments": arguments}),
        )
        .await;
    assert_eq!(created["structuredContent"]["owner_id"], ada.as_str());

    let arguments = json!({"owner_id": "me"});
    let mine = app
        .mcp(
            &ada_key,
            "tools/call",
            json!({"name": "search_contacts", "arguments": arguments}),
        )
        .await;
    assert_eq!(
        mine["structuredContent"]["data"].as_array().unwrap().len(),
        1
    );
}
