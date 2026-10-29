//! API-key credentials resolved from configuration or the environment.

use reqwest::header::HeaderValue;
use snafu::{ResultExt, ensure};
use tokio::sync::RwLock;

use super::{Result, error};

/// Sensitive auth header built from an explicit key or an environment
/// variable; only environment-sourced keys reload.
pub(super) struct ApiKey {
    header: RwLock<HeaderValue>,
    scheme: &'static str,
    variable: Option<String>,
}

impl ApiKey {
    /// Uses `explicit` when set, otherwise reads `variable`.
    /// `scheme` prefixes the header value, such as `"Bearer "`.
    pub(super) fn new(
        explicit: Option<String>,
        variable: &str,
        scheme: &'static str,
    ) -> Result<Self> {
        match explicit {
            Some(key) => Self::explicit(&key, scheme),
            None => Self::from_env(variable, scheme),
        }
    }

    /// Like `new`, but `None` when neither a key nor a variable is configured.
    pub(super) fn optional(
        explicit: Option<String>,
        variable: Option<&str>,
        scheme: &'static str,
    ) -> Result<Option<Self>> {
        match (explicit, variable) {
            (Some(key), _) => Self::explicit(&key, scheme).map(Some),
            (None, Some(variable)) => Self::from_env(variable, scheme).map(Some),
            (None, None) => Ok(None),
        }
    }

    fn explicit(key: &str, scheme: &'static str) -> Result<Self> {
        ensure!(!key.trim().is_empty(), error::EmptyApiKeySnafu);
        Ok(Self {
            header: RwLock::new(header(scheme, key)?),
            scheme,
            variable: None,
        })
    }

    fn from_env(variable: &str, scheme: &'static str) -> Result<Self> {
        Ok(Self {
            header: RwLock::new(header(scheme, &read_env(variable)?)?),
            scheme,
            variable: Some(variable.to_owned()),
        })
    }

    pub(super) async fn header(&self) -> HeaderValue {
        self.header.read().await.clone()
    }

    /// Re-reads an environment-sourced key; explicit keys are left unchanged.
    pub(super) async fn reload(&self) -> Result<()> {
        if let Some(variable) = &self.variable {
            *self.header.write().await = header(self.scheme, &read_env(variable)?)?;
        }
        Ok(())
    }
}

fn read_env(variable: &str) -> Result<String> {
    let key = std::env::var(variable).context(error::CredentialEnvironmentSnafu { variable })?;
    ensure!(!key.trim().is_empty(), error::CredentialsSnafu { variable });
    Ok(key)
}

fn header(scheme: &str, key: &str) -> Result<HeaderValue> {
    let mut header =
        HeaderValue::from_str(&format!("{scheme}{key}")).context(error::CredentialHeaderSnafu)?;
    header.set_sensitive(true);
    Ok(header)
}
