//! MCP server mounted at `/mcp` (streamable HTTP). Tools are curated for agents
//! and call the same service layer as the REST API. There are deliberately no
//! delete tools; deletion is available over REST.

use axum::http::request::Parts;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::Error;
use crate::service::Page;
use crate::service::activities::{self, Activity, ActivityPatch, ActivityQuery, NewActivity};
use crate::service::api_keys::Principal;
use crate::service::companies::{self, Company, CompanyPatch, CompanyQuery, NewCompany};
use crate::service::contacts::{self, Contact, ContactPatch, ContactQuery, NewContact};
use crate::service::deals::{self, Deal, DealPatch, DealQuery, NewDeal};
use crate::service::pipelines::{self, Pipeline};
use crate::service::search::{self, EntityType, SearchResults};

const INSTRUCTIONS: &str = "\
rolo is a CRM. Records: companies, contacts (people, optionally at a company), \
deals (opportunities moving through pipeline stages), and activities (notes, calls, \
emails, meetings and tasks linked to contacts, companies and/or deals).

- IDs are prefixed: com_, con_, deal_, act_, pip_, stg_.
- Use `search` to find anything by name, email, domain or words in notes, then \
fetch details with get_contact / get_company / get_deal.
- Search before creating to avoid duplicates; emails and company domains are unique.
- Deal amounts are integers in minor currency units (cents).
- Call list_pipelines to find stage IDs; move a deal with update_deal + stage_id.
- Log what you did or learned with log_activity; create follow-ups as kind=task \
with due_at, and complete them with update_activity + completed_at.
- Every record has `custom`, a free-form object for extra fields.";

type ToolResult<T> = Result<Json<T>, String>;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Words to find; each matches as a prefix and all must match.
    pub query: String,
    /// Restrict to these record types. Default: all.
    #[serde(default)]
    pub types: Vec<EntityType>,
    /// 1-50, default 20.
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdParam {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Update<P> {
    pub id: String,
    /// Only the fields to change.
    pub changes: P,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PipelineList {
    pub pipelines: Vec<Pipeline>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ContactDetails {
    pub contact: Contact,
    pub company: Option<Company>,
    pub deals: Vec<Deal>,
    /// Newest first, up to 20.
    pub recent_activities: Vec<Activity>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CompanyDetails {
    pub company: Company,
    pub contacts: Vec<Contact>,
    pub deals: Vec<Deal>,
    /// Newest first, up to 20.
    pub recent_activities: Vec<Activity>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DealDetails {
    pub deal: Deal,
    /// Newest first, up to 20.
    pub recent_activities: Vec<Activity>,
}

/// Shown to the agent as the tool error text.
fn fail(err: Error) -> String {
    if let Error::Internal(e) = &err {
        tracing::error!(error = ?e, "internal error in MCP tool");
        return "internal error".into();
    }
    format!("{}: {err}", err.code())
}

fn require_write(parts: &Parts) -> Result<(), String> {
    parts
        .extensions
        .get::<Principal>()
        .ok_or(Error::Unauthorized)
        .and_then(|p| p.require_write())
        .map_err(fail)
}

#[derive(Clone)]
pub struct RoloMcp {
    db: Db,
    tool_router: ToolRouter<Self>,
}

impl RoloMcp {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            tool_router: Self::tool_router(),
        }
    }

    async fn recent_activities(&self, query: ActivityQuery) -> Result<Vec<Activity>, Error> {
        let query = ActivityQuery {
            limit: Some(20),
            ..query
        };
        Ok(activities::list(&self.db, query).await?.data)
    }
}

#[tool_router]
impl RoloMcp {
    /// Search contacts, companies, deals and activities (including notes and custom
    /// fields) by keywords. Returns ranked hits with IDs and a matching snippet.
    #[tool(annotations(read_only_hint = true))]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> ToolResult<SearchResults> {
        search::search(&self.db, &params.query, &params.types, params.limit)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Search contacts by name, email or phone, optionally within a company.
    #[tool(annotations(read_only_hint = true))]
    async fn search_contacts(
        &self,
        Parameters(query): Parameters<ContactQuery>,
    ) -> ToolResult<Page<Contact>> {
        contacts::list(&self.db, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Get a contact with their company, deals and recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_contact(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<ContactDetails> {
        let details = async {
            let contact = contacts::get(&self.db.reader, &id).await?;
            let company = match &contact.company_id {
                Some(company_id) => Some(companies::get(&self.db.reader, company_id).await?),
                None => None,
            };
            let deals = deals::list(
                &self.db,
                DealQuery {
                    contact_id: Some(id.clone()),
                    ..Default::default()
                },
            )
            .await?
            .data;
            let recent_activities = self
                .recent_activities(ActivityQuery {
                    contact_id: Some(id.clone()),
                    ..Default::default()
                })
                .await?;
            Ok(ContactDetails {
                contact,
                company,
                deals,
                recent_activities,
            })
        };
        details.await.map(Json).map_err(fail)
    }

    /// Create a contact. Search first: emails must be unique.
    #[tool]
    async fn create_contact(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NewContact>,
    ) -> ToolResult<Contact> {
        require_write(&parts)?;
        contacts::create(&self.db, input)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Update a contact. Only the fields in `changes` are modified; set a field to null to clear it.
    #[tool(annotations(idempotent_hint = true))]
    async fn update_contact(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Update { id, changes }): Parameters<Update<ContactPatch>>,
    ) -> ToolResult<Contact> {
        require_write(&parts)?;
        contacts::update(&self.db, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Search companies by name or domain.
    #[tool(annotations(read_only_hint = true))]
    async fn search_companies(
        &self,
        Parameters(query): Parameters<CompanyQuery>,
    ) -> ToolResult<Page<Company>> {
        companies::list(&self.db, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Get a company with its contacts, deals and recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_company(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<CompanyDetails> {
        let details = async {
            let company = companies::get(&self.db.reader, &id).await?;
            let contacts = contacts::list(
                &self.db,
                ContactQuery {
                    company_id: Some(id.clone()),
                    ..Default::default()
                },
            )
            .await?
            .data;
            let deals = deals::list(
                &self.db,
                DealQuery {
                    company_id: Some(id.clone()),
                    ..Default::default()
                },
            )
            .await?
            .data;
            let recent_activities = self
                .recent_activities(ActivityQuery {
                    company_id: Some(id.clone()),
                    ..Default::default()
                })
                .await?;
            Ok(CompanyDetails {
                company,
                contacts,
                deals,
                recent_activities,
            })
        };
        details.await.map(Json).map_err(fail)
    }

    /// Create a company. Search first: domains must be unique.
    #[tool]
    async fn create_company(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NewCompany>,
    ) -> ToolResult<Company> {
        require_write(&parts)?;
        companies::create(&self.db, input)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Update a company. Only the fields in `changes` are modified; set a field to null to clear it.
    #[tool(annotations(idempotent_hint = true))]
    async fn update_company(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Update { id, changes }): Parameters<Update<CompanyPatch>>,
    ) -> ToolResult<Company> {
        require_write(&parts)?;
        companies::update(&self.db, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// List sales pipelines and their stages (needed for stage IDs).
    #[tool(annotations(read_only_hint = true))]
    async fn list_pipelines(&self) -> ToolResult<PipelineList> {
        pipelines::list(&self.db)
            .await
            .map(|pipelines| Json(PipelineList { pipelines }))
            .map_err(fail)
    }

    /// Search deals by title, pipeline, stage, status (open/won/lost), company or contact.
    #[tool(annotations(read_only_hint = true))]
    async fn search_deals(
        &self,
        Parameters(query): Parameters<DealQuery>,
    ) -> ToolResult<Page<Deal>> {
        deals::list(&self.db, query).await.map(Json).map_err(fail)
    }

    /// Get a deal with its recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_deal(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<DealDetails> {
        let details = async {
            let deal = deals::get(&self.db.reader, &id).await?;
            let recent_activities = self
                .recent_activities(ActivityQuery {
                    deal_id: Some(id.clone()),
                    ..Default::default()
                })
                .await?;
            Ok(DealDetails {
                deal,
                recent_activities,
            })
        };
        details.await.map(Json).map_err(fail)
    }

    /// Create a deal. It starts in the first open stage unless `stage_id` is given.
    #[tool]
    async fn create_deal(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NewDeal>,
    ) -> ToolResult<Deal> {
        require_write(&parts)?;
        deals::create(&self.db, input).await.map(Json).map_err(fail)
    }

    /// Update a deal, including moving it to another stage via `stage_id`.
    /// Entering a won/lost stage sets `closed_at`.
    #[tool(annotations(idempotent_hint = true))]
    async fn update_deal(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Update { id, changes }): Parameters<Update<DealPatch>>,
    ) -> ToolResult<Deal> {
        require_write(&parts)?;
        deals::update(&self.db, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// List activities newest first, filtered by contact, company, deal or kind.
    /// Use open=true for outstanding tasks.
    #[tool(annotations(read_only_hint = true))]
    async fn list_activities(
        &self,
        Parameters(query): Parameters<ActivityQuery>,
    ) -> ToolResult<Page<Activity>> {
        activities::list(&self.db, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Log a note, call, email or meeting, or create a task (with due_at).
    /// Must link to at least one contact, company or deal.
    #[tool]
    async fn log_activity(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(input): Parameters<NewActivity>,
    ) -> ToolResult<Activity> {
        require_write(&parts)?;
        activities::create(&self.db, input)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Update an activity. Complete a task by setting completed_at; reopen it with null.
    #[tool(annotations(idempotent_hint = true))]
    async fn update_activity(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Update { id, changes }): Parameters<Update<ActivityPatch>>,
    ) -> ToolResult<Activity> {
        require_write(&parts)?;
        activities::update(&self.db, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RoloMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rolo", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
