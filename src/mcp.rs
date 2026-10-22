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
use crate::service::users::{self, User, UserQuery};

const INSTRUCTIONS: &str = "\
rolo is a CRM. Records: companies, contacts (people, optionally at a company), \
deals (opportunities moving through pipeline stages), and activities (notes, calls, \
emails, meetings and tasks linked to contacts, companies and/or deals).

- IDs are prefixed: com_, con_, deal_, act_, pip_, stg_, usr_.
- Use `search` to find anything by name, email, domain or words in notes, then \
fetch details with get_contact / get_company / get_deal.
- Search before creating to avoid duplicates; emails and company domains are unique.
- Deal amounts are integers in minor currency units (cents).
- Call list_pipelines to find stage IDs; move a deal with update_deal + stage_id.
- Log what you did or learned with log_activity; create follow-ups as kind=task \
with due_at, and complete them with update_activity + completed_at.
- Records have an owner_id (for tasks, the assignee). New records default to the \
user this API key belongs to; `\"me\"` means that user anywhere an owner is accepted, \
so list_activities with owner_id=\"me\" and open=true lists your tasks. Call \
list_users to find other users.
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
pub struct UserList {
    /// Active users, by name.
    pub users: Vec<User>,
    /// The user this API key belongs to (`"me"`), if any.
    pub me: Option<String>,
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

/// The authenticated caller, attached by the auth middleware.
fn caller(parts: &Parts) -> Result<&Principal, String> {
    parts
        .extensions
        .get::<Principal>()
        .ok_or_else(|| fail(Error::Unauthorized))
}

/// The caller, if their key may modify data.
fn writer(parts: &Parts) -> Result<&Principal, String> {
    let caller = caller(parts)?;
    caller.require_write().map_err(fail)?;
    Ok(caller)
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

    async fn recent_activities(
        &self,
        caller: &Principal,
        query: ActivityQuery,
    ) -> Result<Vec<Activity>, Error> {
        let query = ActivityQuery {
            limit: Some(20),
            ..query
        };
        Ok(activities::list(&self.db, caller, query).await?.data)
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

    /// List active users (for owner_id), and which one this API key acts as.
    #[tool(annotations(read_only_hint = true))]
    async fn list_users(&self, Extension(parts): Extension<Parts>) -> ToolResult<UserList> {
        let caller = caller(&parts)?;
        let users = users::list(&self.db, UserQuery::default())
            .await
            .map_err(fail)?;
        Ok(Json(UserList {
            users,
            me: caller.user_id.clone(),
        }))
    }

    /// Search contacts by name, email, phone or custom fields, optionally within a
    /// company or by owner.
    #[tool(annotations(read_only_hint = true))]
    async fn search_contacts(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(query): Parameters<ContactQuery>,
    ) -> ToolResult<Page<Contact>> {
        contacts::list(&self.db, caller(&parts)?, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Get a contact with their company, deals and recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_contact(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<ContactDetails> {
        let caller = caller(&parts)?;
        let details = async {
            let contact = contacts::get(&self.db.reader, &id).await?;
            let company = match &contact.company_id {
                Some(company_id) => Some(companies::get(&self.db.reader, company_id).await?),
                None => None,
            };
            let deals_query = DealQuery {
                contact_id: Some(id.clone()),
                ..Default::default()
            };
            let deals = deals::list(&self.db, caller, deals_query).await?.data;
            let activities_query = ActivityQuery {
                contact_id: Some(id.clone()),
                ..Default::default()
            };
            let recent_activities = self.recent_activities(caller, activities_query).await?;
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
        contacts::create(&self.db, writer(&parts)?, input)
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
        contacts::update(&self.db, writer(&parts)?, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Search companies by name, domain or custom fields, optionally by owner.
    #[tool(annotations(read_only_hint = true))]
    async fn search_companies(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(query): Parameters<CompanyQuery>,
    ) -> ToolResult<Page<Company>> {
        companies::list(&self.db, caller(&parts)?, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Get a company with its contacts, deals and recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_company(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<CompanyDetails> {
        let caller = caller(&parts)?;
        let details = async {
            let company = companies::get(&self.db.reader, &id).await?;
            let contacts_query = ContactQuery {
                company_id: Some(id.clone()),
                ..Default::default()
            };
            let contacts = contacts::list(&self.db, caller, contacts_query).await?.data;
            let deals_query = DealQuery {
                company_id: Some(id.clone()),
                ..Default::default()
            };
            let deals = deals::list(&self.db, caller, deals_query).await?.data;
            let activities_query = ActivityQuery {
                company_id: Some(id.clone()),
                ..Default::default()
            };
            let recent_activities = self.recent_activities(caller, activities_query).await?;
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
        companies::create(&self.db, writer(&parts)?, input)
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
        companies::update(&self.db, writer(&parts)?, &id, changes)
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

    /// Search deals by title, pipeline, stage, status (open/won/lost), company,
    /// contact or owner.
    #[tool(annotations(read_only_hint = true))]
    async fn search_deals(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(query): Parameters<DealQuery>,
    ) -> ToolResult<Page<Deal>> {
        deals::list(&self.db, caller(&parts)?, query)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Get a deal with its recent activity.
    #[tool(annotations(read_only_hint = true))]
    async fn get_deal(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> ToolResult<DealDetails> {
        let caller = caller(&parts)?;
        let details = async {
            let deal = deals::get(&self.db.reader, &id).await?;
            let activities_query = ActivityQuery {
                deal_id: Some(id.clone()),
                ..Default::default()
            };
            let recent_activities = self.recent_activities(caller, activities_query).await?;
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
        deals::create(&self.db, writer(&parts)?, input)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// Update a deal, including moving it to another stage via `stage_id`.
    /// Entering a won/lost stage sets `closed_at`.
    #[tool(annotations(idempotent_hint = true))]
    async fn update_deal(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(Update { id, changes }): Parameters<Update<DealPatch>>,
    ) -> ToolResult<Deal> {
        deals::update(&self.db, writer(&parts)?, &id, changes)
            .await
            .map(Json)
            .map_err(fail)
    }

    /// List activities newest first, filtered by contact, company, deal, kind or
    /// owner. Use open=true for outstanding tasks, and owner_id="me" for your own.
    #[tool(annotations(read_only_hint = true))]
    async fn list_activities(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(query): Parameters<ActivityQuery>,
    ) -> ToolResult<Page<Activity>> {
        activities::list(&self.db, caller(&parts)?, query)
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
        activities::create(&self.db, writer(&parts)?, input)
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
        activities::update(&self.db, writer(&parts)?, &id, changes)
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
