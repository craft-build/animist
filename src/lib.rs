//! Model inference and agent building for the Craft ecosystem:
//! provider clients, protocol adapters, revision-preserving history,
//! and a layered agent runtime.
//!
//! Public types are not `#[non_exhaustive]`; new variants or fields are breaking.

pub mod agent;
pub mod catalog;
pub mod history;
pub mod protocol;
pub mod provider;

/// Re-exported because public signatures accept these types.
pub use {flume, jiff, reqwest, serde_json};
#[cfg(feature = "bedrock")]
pub use {http, url};

#[macro_use]
extern crate macro_rules_attribute;

derive_alias! {
 #[derive(Struct!)] = #[derive(Debug, Clone, Default)];
 #[derive(StructWithCustomDefault!)] = #[derive(Debug, Clone)];
 #[derive(StructNoDebug!)] = #[derive(Clone)];
 #[derive(Enum!)] = #[derive(Debug, Clone, PartialEq, Eq)];
}

use snafu::Snafu;

use crate::{agent::AgentError, protocol::ProtocolError, provider::ProviderError};

/// Top-level failures; `Other` hosts user-supplied errors from
/// application callbacks such as history and provider hooks.
#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(transparent)]
    Agent { source: AgentError },
    #[snafu(transparent)]
    Protocol { source: ProtocolError },
    #[snafu(transparent)]
    Provider { source: ProviderError },
    #[snafu(transparent)]
    Catalog { source: crate::catalog::Error },
    #[snafu(transparent)]
    Other {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
