#![forbid(unsafe_code)]

//! Small shared kernel for the Rust platform.
//!
//! It intentionally contains only transport-neutral IDs, error mapping,
//! configuration, request identity, and the membership authorization primitive.
//! Product workflows belong to their owning vertical slice crates.

pub mod authz;
pub mod config;
pub mod error;
pub mod ids;
pub mod input;
pub mod integrity;
pub mod state;
pub mod telemetry;

pub use authz::{Actor, ProjectScope, Role, require_project_role};
pub use config::{AuthenticationAuthority, Config, ConfigError};
pub use error::{AppError, Problem};
pub use ids::{OrganizationId, PageId, ProjectId, UserId};
pub use input::StrictJson;
pub use integrity::canonical_json_sha256;
pub use metrics_exporter_prometheus::PrometheusHandle;
pub use state::AppState;
