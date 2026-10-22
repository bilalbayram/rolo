//! Business logic shared by the REST API and the MCP server. Every CRM
//! mutation runs in a write transaction that also appends to the `events` outbox.

pub mod activities;
pub mod api_keys;
pub mod companies;
pub mod contacts;
pub mod deals;
pub mod events;
mod page;
pub mod pipelines;
pub mod search;
pub mod users;
pub mod webhooks;

pub use page::Page;
