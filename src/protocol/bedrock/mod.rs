//! Bedrock Converse translation and bounded AWS `EventStream` assembly.

pub mod converse;
mod error;
mod eventstream;
mod types;

pub use error::{BedrockProtocolError, EventStreamError};
