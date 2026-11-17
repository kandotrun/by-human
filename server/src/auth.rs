//! App-owned Sign in with ChatGPT. No credentials are read from other tools.
//!
//! Protocol: https://developers.openai.com/siwc/token-sharing-open-source
//! Credentials and the stable host identity live in an owner-only directory.

mod oauth;
mod storage;

use std::{fmt, path::Path, sync::Arc, time::Duration};

pub use oauth::PendingLogin;
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use storage::Directory;

const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const MAX_BODY: usize = 256 * 1024;

/// Safe for diagnostics: no provider body, URL, identity, or token is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    Storage,
    UnsafeStorage,
    Busy,
    NotLoggedIn,
    Cancelled,
    Timeout,
    Connection,
    Provider,
    Reauthorize,
    InvalidState,
    InvalidClient,
    InvalidIdentity,
    MissingPermission,
    InvalidResponse,
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Storage => "ChatGPT credentials could not be read or saved",
            Self::UnsafeStorage => {
                "ChatGPT credential storage must be owner-only and not a symlink"
            }
            Self::Busy => "ChatGPT credential storage is busy; retry later",
            Self::NotLoggedIn => "Sign in with ChatGPT before starting the agent",
            Self::Cancelled => "ChatGPT sign-in was cancelled",
            Self::Timeout => "ChatGPT sign-in timed out",
            Self::Connection => "OpenAI authentication is temporarily unreachable",
            Self::Provider => "OpenAI authentication could not complete; retry later",
            Self::Reauthorize => "The ChatGPT session is no longer usable; sign in again",
            Self::InvalidState => "The ChatGPT callback did not match this sign-in attempt",
            Self::InvalidClient => "The ChatGPT registration did not match the selected client",
            Self::InvalidIdentity => "The ChatGPT identity could not be verified",
            Self::MissingPermission => "Enable ChatGPT plan usage before starting the agent",
            Self::InvalidResponse => "OpenAI returned an invalid authentication response",
        })
    }
}

impl std::error::Error for AuthError {}

type Result<T> = std::result::Result<T, AuthError>;

/// Intentionally not Debug, Serialize, or Display.
pub struct AccessToken(String);

impl AccessToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Contains no account identity or secret values and is safe for CLI output.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct AuthStatus {
    pub registration_saved: bool,
    pub authenticated: bool,
    pub plan_enabled: bool,
    pub expires_at: Option<i64>,
}

#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<Inner>,
}

struct Inner {
    directory: Directory,
    client: Client,
    endpoints: Endpoints,
    refreshes: std::sync::Mutex<RefreshTasks>,
}

struct RefreshTasks {
    tracker: tokio_util::task::TaskTracker,
    closing: bool,
}

#[derive(Clone)]
struct Endpoints {
    authorize: Url,
    token: Url,
    discovery: Url,
    issuer: String,
}

// Never derive Debug: this structure contains renewable credentials.
#[derive(Serialize, Deserialize)]
struct SavedState {
    version: u32,
    host_id: String,
    registration: Option<Registration>,
}

#[derive(Serialize, Deserialize)]
struct Registration {
    client_id: String,
    subject: String,
    issuer: String,
    tokens: Option<Tokens>,
    #[serde(default)]
    pending_refresh: Option<PendingRefresh>,
}

/// A successful token exchange is renewable state even when discovery/JWKS
/// cannot be reached yet. These unverified credentials never leave auth.
#[derive(Serialize, Deserialize)]
struct PendingRefresh {
    response: oauth::TokenResponse,
    received_at: i64,
}

#[derive(Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: String,
    scopes: Vec<String>,
    expires_at: i64,
    nonce: String,
}

impl AuthManager {
    /// `path` is the dedicated app directory, normally ~/.config/by-human.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Self::build(path.as_ref(), Endpoints::production())
    }

    fn build(path: &Path, endpoints: Endpoints) -> Result<Self> {
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("by-human/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| AuthError::Connection)?;
        Ok(Self {
            inner: Arc::new(Inner {
                directory: Directory::open(path)?,
                client,
                endpoints,
                refreshes: std::sync::Mutex::new(RefreshTasks {
                    tracker: tokio_util::task::TaskTracker::new(),
                    closing: false,
                }),
            }),
        })
    }

    /// A fresh registration replaces the selected account only after successful
    /// identity validation. Ordinary login reuses its issued client ID.
    pub async fn begin_login(&self, new_registration: bool) -> Result<PendingLogin> {
        oauth::begin(self.clone(), new_registration, false).await
    }

    /// Explicitly ask to enable plan usage without replacing the saved client.
    /// The user still reviews and grants permission in OpenAI's consent screen.
    pub async fn begin_plan_consent(&self) -> Result<PendingLogin> {
        oauth::begin(self.clone(), false, true).await
    }

    pub async fn status(&self) -> Result<AuthStatus> {
        let _lock = self.inner.directory.lock().await?;
        Ok(status_of(self.inner.directory.read()?.as_ref()))
    }

    /// Refresh is bounded and continues through atomic persistence if its caller
    /// is cancelled. Dropping an awaiting model request must not lose a rotated
    /// refresh token after the provider has accepted it.
    pub async fn access_token(&self) -> Result<AccessToken> {
        let manager = self.clone();
        let task = {
            let refreshes = self
                .inner
                .refreshes
                .lock()
                .map_err(|_| AuthError::Storage)?;
            if refreshes.closing {
                return Err(AuthError::Cancelled);
            }
            refreshes
                .tracker
                .spawn(async move { manager.access_token_locked().await })
        };
        task.await.map_err(|_| AuthError::Storage)?
    }

    /// Call after stopping request producers, before shutting down Tokio. The
    /// mutex prevents shutdown from missing a refresh about to be spawned.
    /// Afterwards this manager refuses new access-token requests.
    pub async fn settle_refreshes(&self) -> Result<()> {
        let tracker = {
            let mut refreshes = self
                .inner
                .refreshes
                .lock()
                .map_err(|_| AuthError::Storage)?;
            refreshes.closing = true;
            refreshes.tracker.close();
            refreshes.tracker.clone()
        };
        tracker.wait().await;
        Ok(())
    }

    async fn access_token_locked(&self) -> Result<AccessToken> {
        let _lock = self.inner.directory.lock().await?;
        let mut saved = self.inner.directory.read()?.ok_or(AuthError::NotLoggedIn)?;
        let registration = saved.registration.as_ref().ok_or(AuthError::NotLoggedIn)?;
        if registration.issuer != self.inner.endpoints.issuer {
            return Err(AuthError::InvalidIdentity);
        }
        // Retry verification from the durable replacement before considering
        // any token use or exchange. The superseded refresh token is not reused.
        self.validate_pending_refresh(&mut saved).await?;
        let registration = saved.registration.as_mut().ok_or(AuthError::NotLoggedIn)?;
        let tokens = registration.tokens.as_ref().ok_or(AuthError::Reauthorize)?;
        if !tokens.scopes.iter().any(|scope| scope == DIRECT_SCOPE) {
            return Err(AuthError::MissingPermission);
        }
        if tokens.expires_at > now() + 60 {
            return Ok(AccessToken(tokens.access_token.clone()));
        }

        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", registration.client_id.as_str()),
            ("refresh_token", tokens.refresh_token.as_str()),
            ("resource", RESOURCE),
        ];
        let response = match self.exchange(&form).await {
            Ok(response) => response,
            Err(AuthError::Reauthorize) => {
                registration.tokens = None;
                self.inner.directory.write(&saved)?;
                return Err(AuthError::Reauthorize);
            }
            Err(error) => return Err(error),
        };
        registration.pending_refresh = Some(PendingRefresh {
            response,
            received_at: now(),
        });
        // No further network await may precede saving a rotated refresh token.
        self.inner.directory.write(&saved)?;
        self.validate_pending_refresh(&mut saved).await?;
        let tokens = saved
            .registration
            .as_ref()
            .and_then(|registration| registration.tokens.as_ref())
            .ok_or(AuthError::Reauthorize)?;
        if !tokens.scopes.iter().any(|scope| scope == DIRECT_SCOPE) {
            return Err(AuthError::MissingPermission);
        }
        if tokens.expires_at <= now() {
            return Err(AuthError::Provider);
        }
        Ok(AccessToken(tokens.access_token.clone()))
    }

    async fn validate_pending_refresh(&self, saved: &mut SavedState) -> Result<()> {
        let registration = saved.registration.as_ref().ok_or(AuthError::NotLoggedIn)?;
        let Some(pending) = registration.pending_refresh.as_ref() else {
            return Ok(());
        };
        let (_, tokens) = self
            .validate_tokens(
                pending.response.clone(),
                &registration.client_id,
                Some(&registration.subject),
                None,
                registration.tokens.as_ref(),
                pending.received_at,
            )
            .await?;
        let registration = saved.registration.as_mut().ok_or(AuthError::NotLoggedIn)?;
        registration.tokens = Some(tokens);
        registration.pending_refresh = None;
        self.inner.directory.write(saved)
    }
}

impl Endpoints {
    fn production() -> Self {
        Self {
            authorize: Url::parse("https://auth.openai.com/api/accounts/authorize")
                .expect("static authorization URL"),
            token: Url::parse("https://auth.openai.com/api/accounts/oauth/token")
                .expect("static token URL"),
            discovery: Url::parse("https://auth.openai.com/.well-known/openid-configuration")
                .expect("static discovery URL"),
            issuer: "https://auth.openai.com".into(),
        }
    }

    fn permits_jwks(&self, url: &Url) -> bool {
        url.origin() == self.discovery.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn status_of(saved: Option<&SavedState>) -> AuthStatus {
    let registration = saved.and_then(|state| state.registration.as_ref());
    let tokens = registration.and_then(|registration| registration.tokens.as_ref());
    AuthStatus {
        registration_saved: registration.is_some(),
        authenticated: tokens.is_some(),
        plan_enabled: tokens
            .is_some_and(|tokens| tokens.scopes.iter().any(|scope| scope == DIRECT_SCOPE)),
        expires_at: tokens.map(|tokens| tokens.expires_at),
    }
}

#[cfg(test)]
mod tests;
