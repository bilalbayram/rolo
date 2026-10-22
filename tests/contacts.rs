mod common;

use axum::http::StatusCode;
use common::TestApp;
use rolo::phone::Region;
use rolo::service::contacts;
use serde_json::{Value, json};

#[tokio::test]
async fn phones_are_normalized_and_unique() {
    let app = TestApp::new().await;

    let (status, ada) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "phone": "0532 123 45 67"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(ada["phone"], "+905321234567");
    let ada_id = ada["id"].as_str().unwrap();

    let (status, body) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Grace", "phone": "+90 (532) 123-45-67"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        "a contact with this phone already exists"
    );

    let (status, body) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Grace", "phone": "call me"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid phone number \"call me\"")
    );

    // A number with a country code keeps it; contacts without phones don't clash.
    let (status, grace) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Grace", "phone": "0044 20 7946 0958"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(grace["phone"], "+442079460958");
    let grace_id = grace["id"].as_str().unwrap();
    for name in ["Alan", "Edsger"] {
        let (status, _) = app.post("/v1/contacts", json!({"first_name": name})).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, _) = app
        .patch(
            &format!("/v1/contacts/{grace_id}"),
            json!({"phone": "05321234567"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Exact filter, normalized the same way.
    for phone in ["0532%20123%2045%2067", "%2B905321234567", "5321234567"] {
        let (status, page) = app.get(&format!("/v1/contacts?phone={phone}")).await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&Value> = page["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| &c["id"])
            .collect();
        assert_eq!(ids, vec![&json!(ada_id)], "{phone}");
    }
    let (status, _) = app.get("/v1/contacts?phone=nope").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Clearing frees the number for someone else.
    let (status, ada) = app
        .patch(&format!("/v1/contacts/{ada_id}"), json!({"phone": " "}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ada["phone"], Value::Null);
    let (status, grace) = app
        .patch(
            &format!("/v1/contacts/{grace_id}"),
            json!({"phone": "0532 123 45 67"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(grace["phone"], "+905321234567");
}

#[tokio::test]
async fn stored_phones_are_normalized_without_losing_data() {
    let app = TestApp::new().await;
    // Numbers as they might have been stored before normalization.
    for (id, phone) in [
        ("con_1", "0532 123 45 67"),
        ("con_2", "+90 532 123 45 67"),
        ("con_3", "not a phone"),
        ("con_4", "+442079460958"),
        ("con_5", "12-34"),
    ] {
        sqlx::query(
            "INSERT INTO contacts (id, first_name, phone, created_at, updated_at)
             VALUES (?, 'Someone', ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(phone)
        .execute(&app.db().writer)
        .await
        .unwrap();
    }

    let report = contacts::normalize_stored_phones(app.db(), Region::TURKEY)
        .await
        .unwrap();
    assert_eq!(report.normalized, 1);
    assert_eq!(report.set_aside, vec!["con_2", "con_3", "con_5"]);

    let expected = [
        ("con_1", json!("+905321234567"), json!({})),
        (
            "con_2",
            Value::Null,
            json!({"legacy_phone": "+90 532 123 45 67"}),
        ),
        ("con_3", Value::Null, json!({"legacy_phone": "not a phone"})),
        ("con_4", json!("+442079460958"), json!({})),
        ("con_5", Value::Null, json!({"legacy_phone": "12-34"})),
    ];
    for (id, phone, custom) in expected {
        let (_, contact) = app.get(&format!("/v1/contacts/{id}")).await;
        assert_eq!(
            (&contact["phone"], &contact["custom"]),
            (&phone, &custom),
            "{id}"
        );
    }

    let (_, events) = app.get("/v1/events?type=contact.updated").await;
    assert_eq!(events["data"].as_array().unwrap().len(), 4);

    // Running again changes nothing.
    let report = contacts::normalize_stored_phones(app.db(), Region::TURKEY)
        .await
        .unwrap();
    assert_eq!((report.normalized, report.set_aside.len()), (0, 0));
}

#[tokio::test]
async fn upsert_creates_then_updates_the_matching_contact() {
    let app = TestApp::new().await;

    let (status, first) = app
        .post(
            "/v1/contacts/upsert",
            json!({"first_name": "Ayşe", "phone": "0532 123 45 67", "custom": {"source": "instagram"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first["created"], true);
    assert_eq!(first["contact"]["phone"], "+905321234567");
    let id = first["contact"]["id"].clone();

    // Same number in another format: only non-blank fields change, custom merges.
    let (status, second) = app
        .post(
            "/v1/contacts/upsert",
            json!({"first_name": " ", "last_name": "Yılmaz", "title": null, "phone": "+90 532 123 45 67",
                   "custom": {"conversation_id": "c_2"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["created"], false);
    let contact = &second["contact"];
    assert_eq!(contact["id"], id);
    assert_eq!(contact["first_name"], "Ayşe");
    assert_eq!(contact["last_name"], "Yılmaz");
    assert_eq!(
        contact["custom"],
        json!({"source": "instagram", "conversation_id": "c_2"})
    );

    // Email is tried before phone by default.
    let (status, _) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Grace", "email": "grace@example.com"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, matched) = app
        .post(
            "/v1/contacts/upsert",
            json!({"email": "Ayse@example.com", "phone": "05321234567"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(matched["contact"]["id"], id);
    assert_eq!(matched["contact"]["email"], "ayse@example.com");
    // A match by email can still clash on phone with another contact.
    let (status, _) = app
        .post(
            "/v1/contacts/upsert",
            json!({"email": "grace@example.com", "phone": "05321234567"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // match_on narrows the keys: by phone only, a new email doesn't create.
    let (status, by_phone) = app
        .post(
            "/v1/contacts/upsert",
            json!({"email": "new@example.com", "phone": "05321234567", "match_on": ["phone"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_phone["contact"]["id"], id);

    let (_, page) = app.get("/v1/contacts").await;
    assert_eq!(page["data"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn upsert_follows_owner_rules() {
    let app = TestApp::new().await;
    let (_, ada) = app
        .post(
            "/v1/users",
            json!({"name": "Ada", "email": "ada@example.com"}),
        )
        .await;
    let ada_key = app.key_for_user(ada["id"].as_str().unwrap()).await;

    let body = json!({"first_name": "Lead", "phone": "05321234567"});
    let (status, created) = app
        .send(
            axum::http::Method::POST,
            "/v1/contacts/upsert",
            Some(&ada_key),
            Some(body),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["contact"]["owner_id"], ada["id"]);

    // On update, an omitted or null owner leaves the owner alone.
    let (status, updated) = app
        .post(
            "/v1/contacts/upsert",
            json!({"phone": "05321234567", "owner_id": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["contact"]["owner_id"], ada["id"]);
}

#[tokio::test]
async fn upsert_matches_on_unique_custom_fields() {
    let app = TestApp::new().await;
    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "contact", "key": "instagram_id", "field_type": "text", "unique": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "contact", "key": "source", "field_type": "text"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let lead = |name: &str| {
        json!({"first_name": name, "custom": {"instagram_id": "1784"},
               "match_on": ["custom.instagram_id", "phone"]})
    };
    let (status, first) = app.post("/v1/contacts/upsert", lead("Ayşe")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, second) = app.post("/v1/contacts/upsert", lead("Ayşe Y.")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["contact"]["id"], first["contact"]["id"]);
    assert_eq!(second["contact"]["first_name"], "Ayşe Y.");

    for (match_on, problem) in [
        (json!(["custom.source"]), "cannot match on custom.source"),
        (json!(["custom.missing"]), "cannot match on custom.missing"),
        (json!(["name"]), "cannot match on \"name\""),
        (json!([]), "match_on must list at least one key"),
    ] {
        let (status, body) = app
            .post(
                "/v1/contacts/upsert",
                json!({"first_name": "X", "match_on": match_on}),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{match_on}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(problem), "{message}");
    }
}

#[tokio::test]
async fn upsert_contact_tool() {
    let app = TestApp::new().await;
    let call = |phone: &str| {
        json!({"name": "upsert_contact",
               "arguments": {"first_name": "Ayşe", "phone": phone}})
    };
    let first = app
        .mcp(&app.token, "tools/call", call("0532 123 45 67"))
        .await;
    assert_ne!(first["isError"], true, "{first}");
    assert_eq!(first["structuredContent"]["created"], true);
    let second = app.mcp(&app.token, "tools/call", call("5321234567")).await;
    assert_eq!(second["structuredContent"]["created"], false);
    assert_eq!(
        second["structuredContent"]["contact"]["id"],
        first["structuredContent"]["contact"]["id"]
    );

    let denied = app
        .mcp(&app.read_token, "tools/call", call("05321234567"))
        .await;
    assert_eq!(denied["isError"], true);
}
