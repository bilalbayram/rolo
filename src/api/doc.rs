use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Top-level OpenAPI metadata; operations are added by the routers.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "rolo",
        description = "Headless CRM API. Authenticate with `Authorization: Bearer <api key>`. \
                       Every mutation is also published to `/v1/events` and to webhooks."
    ),
    modifiers(&BearerAuth),
    security(("bearer" = [])),
    tags(
        (name = "search", description = "Full-text search across all records"),
        (name = "companies"),
        (name = "contacts"),
        (name = "deals"),
        (name = "pipelines"),
        (name = "users", description = "People who own records; API keys can act as a user"),
        (name = "activities", description = "Notes, calls, emails, meetings and tasks"),
        (name = "events", description = "Change feed of every mutation, for sync and integrations"),
        (name = "webhooks", description = "Signed HTTP callbacks for events, with retries"),
    )
)]
pub struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}
