use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The kinds of CRM record that are searchable and can have custom fields.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    ToSchema,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum EntityType {
    Contact,
    Company,
    Deal,
    Activity,
}

impl EntityType {
    /// Lowercase name, as in the API, e.g. `contact`.
    pub fn name(self) -> &'static str {
        match self {
            EntityType::Contact => "contact",
            EntityType::Company => "company",
            EntityType::Deal => "deal",
            EntityType::Activity => "activity",
        }
    }
}
