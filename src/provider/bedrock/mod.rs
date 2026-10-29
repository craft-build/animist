//! Bedrock native API routing with AWS credentials and `SigV4` authentication.

mod error;
mod listing;
mod provider;

pub use aws_credential_types::{
    Credentials,
    provider::{
        ProvideCredentials, SharedCredentialsProvider, error::CredentialsError,
        future::ProvideCredentials as CredentialsFuture,
    },
};
pub use aws_sigv4::{
    http_request::SigningError, sign::v4::signing_params::BuildError as SigningParametersError,
};
pub use aws_types::{SdkConfig, region::Region};
pub use error::BedrockError;
pub use provider::{Bedrock, BedrockConfig, ListingMode, Protocol};
