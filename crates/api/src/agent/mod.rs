//! Agent transport — see `internal-docs/agent-transport.md`.

pub mod auth;
pub mod link;
pub mod registry;
pub mod routes;

pub use registry::AgentRegistry;
