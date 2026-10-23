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

#[tokio::test]
async fn unique_fields_reject_values_another_record_has() {
    let app = TestApp::new().await;
    let definition = define(
        &app,
        json!({"entity_type": "contact", "key": "instagram_id", "field_type": "text", "unique": true}),
    )
    .await;
    assert_eq!(definition["unique"], true);

    let contact = |name: &str, instagram_id: &str| json!({"first_name": name, "custom": {"instagram_id": instagram_id}});
    let (status, ada) = app.post("/v1/contacts", contact("Ada", "ig_1")).await;
    assert_eq!(status, StatusCode::CREATED);
    let ada_id = ada["id"].as_str().unwrap();
    let (status, body) = app.post("/v1/contacts", contact("Grace", "ig_1")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        message(&body),
        "custom.instagram_id: another contact has this value"
    );
    let (status, grace) = app.post("/v1/contacts", contact("Grace", "ig_2")).await;
    assert_eq!(status, StatusCode::CREATED);
    let grace_url = format!("/v1/contacts/{}", grace["id"].as_str().unwrap());

    let instagram_id = |value: Value| json!({"custom": {"instagram_id": value}});
    let (status, _) = app.patch(&grace_url, instagram_id(json!("ig_1"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Writing a record's own value again is fine.
    let ada_url = format!("/v1/contacts/{ada_id}");
    let (status, _) = app.patch(&ada_url, instagram_id(json!("ig_1"))).await;
    assert_eq!(status, StatusCode::OK);
    // Removing a value frees it.
    let (status, _) = app.patch(&ada_url, instagram_id(Value::Null)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app.patch(&grace_url, instagram_id(json!("ig_1"))).await;
    assert_eq!(status, StatusCode::OK);

    // Uniqueness is per record type.
    let (status, _) = app
        .post(
            "/v1/companies",
            json!({"name": "Acme", "custom": {"instagram_id": "ig_1"}}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // `unique` is fixed once defined.
    let definition_url = format!("/v1/custom-fields/{}", definition["id"].as_str().unwrap());
    let (status, _) = app.patch(&definition_url, json!({"unique": false})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn unique_fields_need_a_suitable_type_and_no_existing_duplicates() {
    let app = TestApp::new().await;
    let (status, body) = app
        .post(
            "/v1/custom-fields",
            json!({"entity_type": "contact", "key": "vip", "field_type": "boolean", "unique": true}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        message(&body),
        "boolean and multiselect fields cannot be unique"
    );

    for _ in 0..2 {
        let (status, _) = app
            .post(
                "/v1/deals",
                json!({"title": "Wedding", "custom": {"booking_ref": 7}}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let unique_ref = json!({"entity_type": "deal", "key": "booking_ref", "field_type": "number", "unique": true});
    let (status, body) = app.post("/v1/custom-fields", unique_ref.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        message(&body),
        "custom.booking_ref cannot be unique: some records already share a value"
    );
    let (_, page) = app.get("/v1/deals").await;
    let second = page["data"][1]["id"].as_str().unwrap();
    let (status, _) = app
        .patch(
            &format!("/v1/deals/{second}"),
            json!({"custom": {"booking_ref": 8}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    define(&app, unique_ref).await;
    let (status, _) = app
        .post(
            "/v1/deals",
            json!({"title": "Gala", "custom": {"booking_ref": 8.0}}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

fn ids(page: &Value) -> Vec<&str> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn lists_filter_on_custom_fields() {
    let app = TestApp::new().await;
    for definition in [
        json!({"entity_type": "deal", "key": "event_type", "field_type": "select",
               "options": ["dugun_nisan", "sirket_yemegi"]}),
        json!({"entity_type": "deal", "key": "mihrabat_inquiry_id", "field_type": "number", "unique": true}),
        json!({"entity_type": "deal", "key": "vip", "field_type": "boolean"}),
        json!({"entity_type": "deal", "key": "tags", "field_type": "multiselect", "options": ["a"]}),
        json!({"entity_type": "contact", "key": "source", "field_type": "text"}),
    ] {
        define(&app, definition).await;
    }
    let mut deals = Vec::new();
    for custom in [
        json!({"event_type": "dugun_nisan", "mihrabat_inquiry_id": 123, "vip": true}),
        json!({"event_type": "sirket_yemegi", "mihrabat_inquiry_id": 124}),
        json!({"event_type": "dugun_nisan"}),
    ] {
        let (status, deal) = app
            .post("/v1/deals", json!({"title": "Event", "custom": custom}))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{deal}");
        deals.push(deal["id"].as_str().unwrap().to_string());
    }

    for (query, expected) in [
        ("custom.event_type=dugun_nisan", vec![&deals[0], &deals[2]]),
        (
            "custom.event_type=dugun_nisan&sort=-id",
            vec![&deals[2], &deals[0]],
        ),
        (
            "custom.event_type=dugun_nisan&custom.mihrabat_inquiry_id=123",
            vec![&deals[0]],
        ),
        ("custom.mihrabat_inquiry_id=124.0", vec![&deals[1]]),
        ("custom.vip=true", vec![&deals[0]]),
        ("custom.mihrabat_inquiry_id=999", vec![]),
    ] {
        let (status, page) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), expected, "{query}");
    }

    for (query, problem) in [
        (
            "custom.event_type=gala",
            "custom.event_type: must be one of",
        ),
        (
            "custom.budget=1",
            "custom.budget: not a custom field defined for deals",
        ),
        (
            "custom.tags=a",
            "custom.tags: multiselect fields can only be filtered with has or missing",
        ),
        ("custom.vip=yes", "custom.vip: must be true or false"),
        (
            "custom.vip=true&custom.vip=false",
            "query parameter \"custom.vip\" is given more than once",
        ),
        ("custom.=x", "unknown query parameter \"custom.\""),
        ("customer=x", "unknown query parameter \"customer\""),
    ] {
        let (status, body) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(message(&body).contains(problem), "{query}: {body}");
    }

    let (_, ada) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "custom": {"source": "instagram"}}),
        )
        .await;
    app.post(
        "/v1/contacts",
        json!({"first_name": "Grace", "custom": {"source": "web"}}),
    )
    .await;
    let (_, page) = app.get("/v1/contacts?custom.source=instagram").await;
    assert_eq!(ids(&page), [ada["id"].as_str().unwrap()]);
    let (status, _) = app.get("/v1/contacts?custom.event_type=x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The MCP search tools take the same filters as an object.
    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "search_deals", "arguments": {"custom": {"mihrabat_inquiry_id": 123}}}),
        )
        .await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(ids(&result["structuredContent"]), [deals[0].as_str()]);
    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "search_contacts", "arguments": {"custom": {"budget": 1}}}),
        )
        .await;
    assert_eq!(result["isError"], true);
}

/// Deal fields for the list filter tests: `event_type` (select), `guests`
/// (number), `details` (text) and `tags` (multiselect).
async fn define_deal_fields(app: &TestApp) {
    for definition in [
        json!({"entity_type": "deal", "key": "event_type", "field_type": "select",
               "options": ["dugun_nisan", "sirket_yemegi", "ozel_davet", "a,b"]}),
        json!({"entity_type": "deal", "key": "guests", "field_type": "number"}),
        json!({"entity_type": "deal", "key": "details", "field_type": "text"}),
        json!({"entity_type": "deal", "key": "tags", "field_type": "multiselect", "options": ["x"]}),
    ] {
        define(app, definition).await;
    }
}

async fn create_deal(app: &TestApp, body: Value) -> String {
    let (status, deal) = app.post("/v1/deals", body).await;
    assert_eq!(status, StatusCode::CREATED, "{deal}");
    deal["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn lists_match_any_of_several_values() {
    let app = TestApp::new().await;
    define_deal_fields(&app).await;
    let (_, ada) = app.post("/v1/contacts", json!({"first_name": "Ada"})).await;
    let (_, grace) = app
        .post("/v1/contacts", json!({"first_name": "Grace"}))
        .await;
    let (ada, grace) = (ada["id"].as_str().unwrap(), grace["id"].as_str().unwrap());
    let d = [
        create_deal(
            &app,
            json!({"title": "A", "contact_id": ada,
                   "custom": {"event_type": "dugun_nisan", "guests": 200, "details": "kır, açık büfe"}}),
        )
        .await,
        create_deal(
            &app,
            json!({"title": "B", "contact_id": grace, "stage_id": "stg_qualified",
                   "custom": {"event_type": "sirket_yemegi", "guests": 80}}),
        )
        .await,
        create_deal(
            &app,
            json!({"title": "C", "stage_id": "stg_won", "custom": {"event_type": "ozel_davet"}}),
        )
        .await,
        create_deal(&app, json!({"title": "D", "custom": {"event_type": "a,b"}})).await,
    ];

    for (query, expected, total) in [
        (
            "custom.event_type=dugun_nisan,ozel_davet",
            vec![&d[0], &d[2]],
            2,
        ),
        (
            "custom.event_type=sirket_yemegi, ozel_davet",
            vec![&d[1], &d[2]],
            2,
        ),
        // An option containing a comma still matches as a whole.
        ("custom.event_type=a,b", vec![&d[3]], 1),
        ("custom.guests=80,200", vec![&d[0], &d[1]], 2),
        // Text values may contain commas, so they always match whole.
        ("custom.details=kır, açık büfe", vec![&d[0]], 1),
        ("custom.details=kır", vec![], 0),
        ("stage_id=stg_qualified,stg_won", vec![&d[1], &d[2]], 2),
        ("stage_id=stg_lead", vec![&d[0], &d[3]], 2),
        (
            "stage_id=stg_qualified,stg_won&custom.event_type=ozel_davet,dugun_nisan",
            vec![&d[2]],
            1,
        ),
        (&format!("contact_id={ada},{grace}"), vec![&d[0], &d[1]], 2),
        (&format!("contact_id={grace}"), vec![&d[1]], 1),
    ] {
        let (status, page) = app
            .get(&format!(
                "/v1/deals?{}&include_total=true",
                query.replace(' ', "%20")
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), expected, "{query}");
        assert_eq!(page["total"], total, "{query}");
    }

    for (query, problem) in [
        (
            "custom.event_type=dugun_nisan,gala",
            "custom.event_type: must be one of",
        ),
        ("custom.guests=80,many", "custom.guests: must be a number"),
        ("stage_id=,", "stage_id must list at least one value"),
    ] {
        let (status, body) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(message(&body).contains(problem), "{query}: {body}");
    }

    // Over MCP, an array matches any of its values.
    let result = app
        .mcp(
            &app.token,
            "tools/call",
            json!({"name": "search_deals",
                   "arguments": {"custom": {"event_type": ["sirket_yemegi", "a,b"]}}}),
        )
        .await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(
        ids(&result["structuredContent"]),
        [d[1].as_str(), d[3].as_str()]
    );
}

#[tokio::test]
async fn lists_filter_on_whether_custom_fields_are_set() {
    let app = TestApp::new().await;
    define_deal_fields(&app).await;
    define(
        &app,
        json!({"entity_type": "contact", "key": "instagram_id", "field_type": "text"}),
    )
    .await;
    let d = [
        create_deal(&app, json!({"title": "Raw lead"})).await,
        create_deal(
            &app,
            json!({"title": "Inquiry", "stage_id": "stg_qualified",
                   "custom": {"event_type": "dugun_nisan", "tags": ["x"]}}),
        )
        .await,
        create_deal(
            &app,
            json!({"title": "Raw lead 2", "custom": {"guests": 50}}),
        )
        .await,
    ];
    // A field cleared with null is no longer set.
    let (status, _) = app
        .patch(
            &format!("/v1/deals/{}", d[2]),
            json!({"custom": {"event_type": "ozel_davet"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app
        .patch(
            &format!("/v1/deals/{}", d[2]),
            json!({"custom": {"event_type": null}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    for (query, expected, total) in [
        ("missing=custom.event_type", vec![&d[0], &d[2]], 2),
        ("has=custom.event_type", vec![&d[1]], 1),
        ("has=custom.tags", vec![&d[1]], 1),
        ("missing=custom.event_type,custom.guests,", vec![&d[0]], 1),
        (
            "missing=custom.event_type&has=custom.guests",
            vec![&d[2]],
            1,
        ),
        ("missing=custom.event_type&sort=-id&limit=1", vec![&d[2]], 2),
        (
            "missing=custom.event_type&stage_id=stg_qualified",
            vec![],
            0,
        ),
        (
            "has=custom.event_type&custom.event_type=dugun_nisan",
            vec![&d[1]],
            1,
        ),
    ] {
        let (status, page) = app
            .get(&format!("/v1/deals?{query}&include_total=true"))
            .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), expected, "{query}");
        assert_eq!(page["total"], total, "{query}");
    }

    for (query, problem) in [
        (
            "missing=event_type",
            "\"event_type\": expected custom.<key>",
        ),
        (
            "has=custom.budget",
            "custom.budget: not a custom field defined for deals",
        ),
        (
            "has=custom.instagram_id",
            "custom.instagram_id: not a custom field defined for deals",
        ),
    ] {
        let (status, body) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(message(&body).contains(problem), "{query}: {body}");
    }

    let (_, ada) = app
        .post(
            "/v1/contacts",
            json!({"first_name": "Ada", "custom": {"instagram_id": "17841"}}),
        )
        .await;
    let (_, grace) = app
        .post("/v1/contacts", json!({"first_name": "Grace"}))
        .await;
    let (_, page) = app
        .get("/v1/contacts?has=custom.instagram_id&include_total=true")
        .await;
    assert_eq!(
        (ids(&page), &page["total"]),
        (vec![ada["id"].as_str().unwrap()], &json!(1))
    );
    let (_, page) = app.get("/v1/contacts?missing=custom.instagram_id").await;
    assert_eq!(ids(&page), [grace["id"].as_str().unwrap()]);
}

#[tokio::test]
async fn lists_sum_a_number_field_on_request() {
    let app = TestApp::new().await;
    define_deal_fields(&app).await;
    for custom in [
        json!({"event_type": "dugun_nisan", "guests": 200}),
        json!({"event_type": "dugun_nisan", "guests": 150.5}),
        json!({"event_type": "ozel_davet", "guests": 40}),
        json!({"event_type": "dugun_nisan"}),
    ] {
        create_deal(&app, json!({"title": "Event", "custom": custom})).await;
    }

    let (_, page) = app.get("/v1/deals?limit=1").await;
    assert!(!page.as_object().unwrap().contains_key("sums"));

    for (query, sum) in [
        ("sum=custom.guests", 390.5),
        (
            "sum=custom.guests&custom.event_type=dugun_nisan&limit=1",
            350.5,
        ),
        ("sum=custom.guests&missing=custom.guests", 0.0),
        ("sum=custom.guests&q=nothing", 0.0),
        ("sum=custom.guests&q=--", 0.0),
    ] {
        let (status, page) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(page["sums"], json!({"custom.guests": sum}), "{query}");
        assert!(!page.as_object().unwrap().contains_key("total"), "{query}");
    }
    let (_, page) = app
        .get("/v1/deals?sum=custom.guests&include_total=true")
        .await;
    assert_eq!(
        (&page["total"], &page["sums"]["custom.guests"]),
        (&json!(4), &json!(390.5))
    );

    for (query, problem) in [
        (
            "sum=custom.event_type",
            "sum custom.event_type: not a number field",
        ),
        ("sum=guests", "sum \"guests\": expected custom.<key>"),
        (
            "sum=custom.budget",
            "custom.budget: not a custom field defined for deals",
        ),
        ("sum=custom.guests&ids=deal_x", "ids can't be combined"),
    ] {
        let (status, body) = app.get(&format!("/v1/deals?{query}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert!(message(&body).contains(problem), "{query}: {body}");
    }
}
