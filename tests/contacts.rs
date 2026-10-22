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
