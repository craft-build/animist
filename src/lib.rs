pub mod catalog;
pub mod history;
pub mod protocol;
pub mod provider;

#[macro_use]
extern crate macro_rules_attribute;

derive_alias! {
 #[derive(Struct!)] = #[derive(Debug, Clone, Default, PartialEq)];
 #[derive(StructNoDebug!)] = #[derive(Clone, PartialEq)];
 #[derive(Enum!)] = #[derive(Debug, Clone, PartialEq, Eq)];
}

use snafu::Snafu;

use crate::{protocol::ProtocolError, provider::ProviderError};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(transparent)]
    Protocol { source: ProtocolError },
    #[snafu(transparent)]
    Provider { source: ProviderError },
    #[snafu(transparent)]
    Other {
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
