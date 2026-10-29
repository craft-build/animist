use snafu::Snafu;

/// Bedrock-specific configuration, credential, signing, and transport failures.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum BedrockError {
    #[snafu(display("AWS region was not configured"))]
    MissingRegion { backtrace: snafu::Backtrace },
    #[snafu(display("AWS credential provider was not configured"))]
    MissingCredentialsProvider { backtrace: snafu::Backtrace },
    #[snafu(display("AWS region {region} cannot be used as an endpoint label"))]
    InvalidRegion {
        region: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Bedrock max_attempts must be between 1 and 10, got {attempts}"))]
    InvalidMaxAttempts {
        attempts: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid Bedrock endpoint: {source}"))]
    Endpoint { source: url::ParseError },
    #[snafu(display(
        "Bedrock endpoint must be an HTTPS origin (HTTP only on loopback), without credentials, query, or fragment"
    ))]
    InvalidEndpoint { backtrace: snafu::Backtrace },
    #[snafu(display("Invalid Converse model ID {id:?}"))]
    InvalidConverseModelId {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to create Bedrock HTTP client: {source}"))]
    Client { source: reqwest::Error },
    #[snafu(display("Failed to serialize Bedrock request: {source}"))]
    EncodeRequest { source: serde_json::Error },
    #[snafu(display("Failed to decode Bedrock model listing: {source}"))]
    DecodeListing { source: serde_json::Error },
    #[snafu(display("Bedrock model discovery exceeds {limit} response bytes"))]
    ListingTooLarge {
        limit: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Bedrock model listing returned an empty or repeated token {token:?}"))]
    InvalidListingToken {
        token: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Bedrock model listing returned an empty model ID"))]
    EmptyListedModelId { backtrace: snafu::Backtrace },
    #[snafu(display("Failed to construct Bedrock HTTP request: {source}"))]
    Request { source: http::Error },
    #[snafu(display("Failed to resolve AWS credentials: {source}"))]
    Credentials {
        source: aws_credential_types::provider::error::CredentialsError,
    },
    #[snafu(display("Failed to configure AWS signing: {source}"))]
    SigningParameters {
        source: aws_sigv4::sign::v4::signing_params::BuildError,
    },
    #[snafu(display("Failed to sign Bedrock request: {source}"))]
    Signing {
        source: aws_sigv4::http_request::SigningError,
    },
    #[snafu(display("Signing header is not valid text: {source}"))]
    SigningHeader { source: http::header::ToStrError },
    #[snafu(display("AWS signing produced an invalid {header} header: {source}"))]
    SignedHeaderValue {
        header: String,
        source: http::header::InvalidHeaderValue,
    },
    #[snafu(display("Bedrock HTTP request failed: {source}"))]
    Transport { source: reqwest::Error },
    #[snafu(display(
        "Bedrock returned HTTP {status} (request ID {request_id:?}, error type {error_type:?})"
    ))]
    Http {
        status: u16,
        request_id: Option<String>,
        error_type: Option<String>,
        body: Vec<u8>,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Bedrock HTTP {status} error body exceeds {limit} bytes (request ID {request_id:?})"
    ))]
    ErrorBodyTooLarge {
        status: u16,
        request_id: Option<String>,
        error_type: Option<String>,
        body_prefix: Vec<u8>,
        limit: usize,
        backtrace: snafu::Backtrace,
    },
}
