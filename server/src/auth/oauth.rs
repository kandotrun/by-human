use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    AuthError, AuthManager, AuthStatus, DYNAMIC_CLIENT, MAX_BODY, RESOURCE, Registration, Result,
    SCOPES, SavedState, Tokens, now, status_of,
};

/// Owns its loopback listener before authorization is shown to the user.
/// Intentionally no Debug: transient callback state and codes remain private.
pub struct PendingLogin {
    manager: AuthManager,
    listener: TcpListener,
    attempt: Attempt,
    authorization_url: String,
}

struct Attempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
    selected: Option<(String, String)>,
    existing: Option<(String, String)>,
    host_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: String,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: i64,
    nbf: Option<i64>,
    nonce: Option<String>,
    aud: Value,
    azp: Option<String>,
}

struct Callback {
    code: String,
    client_id: String,
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    expected_host: String,
    selected_client: Option<String>,
    sender: Arc<tokio::sync::Mutex<Option<oneshot::Sender<Result<Callback>>>>>,
}

pub(super) async fn begin(
    manager: AuthManager,
    new_registration: bool,
    request_consent: bool,
) -> Result<PendingLogin> {
    let (host_id, existing) = {
        let _lock = manager.inner.directory.lock().await?;
        let saved = match manager.inner.directory.read()? {
            Some(saved) => saved,
            None => {
                let saved = SavedState {
                    version: 1,
                    host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
                    registration: None,
                };
                // Host identity is durable before authorization begins.
                manager.inner.directory.write(&saved)?;
                saved
            }
        };
        let existing = saved
            .registration
            .map(|registration| (registration.client_id, registration.subject));
        (saved.host_id, existing)
    };
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| AuthError::Connection)?;
    let port = listener
        .local_addr()
        .map_err(|_| AuthError::Connection)?
        .port();
    let selected = if new_registration {
        None
    } else {
        existing.clone()
    };
    let attempt = Attempt {
        state: random()?,
        nonce: random()?,
        verifier: random()?,
        redirect_uri: format!("http://127.0.0.1:{port}/auth/callback"),
        selected,
        existing,
        host_id,
    };
    let mut authorization = manager.inner.endpoints.authorize.clone();
    {
        let mut query = authorization.query_pairs_mut();
        query
            .append_pair(
                "client_id",
                attempt
                    .selected
                    .as_ref()
                    .map_or(DYNAMIC_CLIENT, |(client, _)| client),
            )
            .append_pair("ext_agent_host_id", &attempt.host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &attempt.redirect_uri)
            .append_pair("scope", SCOPES)
            .append_pair("resource", RESOURCE)
            .append_pair("state", &attempt.state)
            .append_pair("nonce", &attempt.nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair(
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.verifier.as_bytes())),
            );
        if attempt.selected.is_none() {
            query.append_pair("agent_name_hint", "by-human");
        }
        if request_consent {
            query.append_pair("prompt", "consent");
        }
        // Deliberately omit token/login hints: the URL is safe to display in a
        // terminal. Returning sign-in still selects the saved registration.
    }
    Ok(PendingLogin {
        manager,
        listener,
        attempt,
        authorization_url: authorization.into(),
    })
}

impl PendingLogin {
    /// Contains PKCE challenge/state/nonce, but never an ID/access/refresh token.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Wait up to five minutes. Cancellation before code exchange closes the
    /// listener. Once exchange starts, verification and persistence finish.
    pub async fn complete(self, cancel: &CancellationToken) -> Result<AuthStatus> {
        self.complete_with_timeout(cancel, Duration::from_secs(300))
            .await
    }

    pub(super) async fn complete_with_timeout(
        self,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<AuthStatus> {
        let (sender, receiver) = oneshot::channel();
        let expected_host =
            Url::parse(&self.attempt.redirect_uri).map_err(|_| AuthError::Connection)?;
        let state = CallbackState {
            expected_state: self.attempt.state.clone(),
            expected_host: format!("127.0.0.1:{}", expected_host.port().unwrap_or_default()),
            selected_client: self
                .attempt
                .selected
                .as_ref()
                .map(|(client, _)| client.clone()),
            sender: Arc::new(tokio::sync::Mutex::new(Some(sender))),
        };
        let app = Router::new()
            .route("/auth/callback", get(callback))
            .with_state(state);
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let mut server = tokio::spawn(async move {
            axum::serve(self.listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
        });
        let received = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AuthError::Cancelled),
            result = receiver => result.map_err(|_| AuthError::Connection).and_then(|value|value),
            _ = tokio::time::sleep(timeout) => Err(AuthError::Timeout),
            _ = &mut server => Err(AuthError::Connection),
        };
        stop.cancel();
        if !server.is_finished()
            && tokio::time::timeout(Duration::from_secs(1), &mut server)
                .await
                .is_err()
        {
            server.abort();
        }
        let received = received?;
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        self.manager.finish_login(self.attempt, received).await
    }
}

async fn callback(
    State(state): State<CallbackState>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> (StatusCode, [(&'static str, &'static str); 3], &'static str) {
    let privacy_headers = [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ];
    let parsed = (|| {
        if headers.get("host").and_then(|header| header.to_str().ok())
            != Some(state.expected_host.as_str())
        {
            return Err(AuthError::InvalidState);
        }
        let raw = raw.as_deref().ok_or(AuthError::InvalidState)?;
        if raw.len() > 16 * 1024 {
            return Err(AuthError::InvalidState);
        }
        let parsed =
            Url::parse(&format!("http://127.0.0.1/?{raw}")).map_err(|_| AuthError::InvalidState)?;
        let mut values = HashMap::new();
        for (key, value) in parsed.query_pairs() {
            if values
                .insert(key.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(AuthError::InvalidState);
            }
        }
        let state_value = values.get("state").ok_or(AuthError::InvalidState)?;
        if !constant_eq(state_value, &state.expected_state) {
            return Err(AuthError::InvalidState);
        }
        if values.contains_key("error") {
            return Ok(Err(AuthError::Cancelled));
        }
        let client = match (&state.selected_client, values.get("client_id")) {
            (Some(selected), Some(returned)) if selected != returned => {
                return Ok(Err(AuthError::InvalidClient));
            }
            (Some(selected), _) => selected.clone(),
            (None, Some(returned)) => returned.clone(),
            (None, None) => return Ok(Err(AuthError::InvalidClient)),
        };
        if client == DYNAMIC_CLIENT || !valid_identifier(&client) {
            return Ok(Err(AuthError::InvalidClient));
        }
        let code = values
            .get("code")
            .ok_or(AuthError::InvalidResponse)?
            .clone();
        if code.is_empty() || code.len() > 8192 {
            return Err(AuthError::InvalidResponse);
        }
        Ok(Ok(Callback {
            code,
            client_id: client,
        }))
    })();
    match parsed {
        Ok(result) => {
            if let Some(sender) = state.sender.lock().await.take() {
                let _ = sender.send(result);
                (
                    StatusCode::OK,
                    privacy_headers,
                    "Authorization received. Return to by-human to finish sign-in.",
                )
            } else {
                (
                    StatusCode::GONE,
                    privacy_headers,
                    "This sign-in attempt has already completed.",
                )
            }
        }
        Err(_) => (
            StatusCode::BAD_REQUEST,
            privacy_headers,
            "This callback did not match the pending sign-in attempt.",
        ),
    }
}

impl AuthManager {
    async fn finish_login(&self, attempt: Attempt, callback: Callback) -> Result<AuthStatus> {
        let _lock = self.inner.directory.lock().await?;
        let mut saved = self.inner.directory.read()?.ok_or(AuthError::Storage)?;
        let current = saved
            .registration
            .as_ref()
            .map(|registration| (registration.client_id.clone(), registration.subject.clone()));
        if current != attempt.existing || saved.host_id != attempt.host_id {
            return Err(AuthError::InvalidClient);
        }
        let response = self
            .exchange(&[
                ("grant_type", "authorization_code"),
                ("client_id", callback.client_id.as_str()),
                ("code", callback.code.as_str()),
                ("code_verifier", attempt.verifier.as_str()),
                ("redirect_uri", attempt.redirect_uri.as_str()),
                ("resource", RESOURCE),
            ])
            .await?;
        let received_at = now();
        let (subject, tokens) = self
            .validate_tokens(
                response,
                &callback.client_id,
                attempt
                    .selected
                    .as_ref()
                    .map(|(_, subject)| subject.as_str()),
                Some(&attempt.nonce),
                None,
                received_at,
            )
            .await?;
        saved.registration = Some(Registration {
            client_id: callback.client_id,
            subject,
            issuer: self.inner.endpoints.issuer.clone(),
            tokens: Some(tokens),
            pending_refresh: None,
        });
        self.inner.directory.write(&saved)?;
        Ok(status_of(Some(&saved)))
    }

    pub(super) async fn exchange(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let response = self
            .inner
            .client
            .post(self.inner.endpoints.token.clone())
            .form(form)
            .send()
            .await
            .map_err(|_| AuthError::Connection)?;
        let status = response.status();
        let bytes = bounded_body(response).await?;
        if status != StatusCode::OK {
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let code = body.get("error").and_then(Value::as_str).unwrap_or("");
            return Err(match code {
                "invalid_grant"
                | "invalid_refresh_token"
                | "token_expired"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
                | "refresh_token_reused" => AuthError::Reauthorize,
                "invalid_client" => AuthError::InvalidClient,
                _ => AuthError::Provider,
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)
    }

    pub(super) async fn validate_tokens(
        &self,
        response: TokenResponse,
        client: &str,
        expected_subject: Option<&str>,
        expected_nonce: Option<&str>,
        previous: Option<&Tokens>,
        received_at: i64,
    ) -> Result<(String, Tokens)> {
        if !response.token_type.eq_ignore_ascii_case("bearer")
            || !valid_token(&response.access_token)
            || !valid_token(&response.refresh_token)
            || !(1..=86400).contains(&response.expires_in)
            || response.scope.len() > 8192
        {
            return Err(AuthError::InvalidResponse);
        }
        let (subject, id_token) = if let Some(id_token) = response.id_token {
            if id_token.is_empty() || id_token.len() > 64 * 1024 {
                return Err(AuthError::InvalidIdentity);
            }
            let claims = self
                .verify_identity(
                    &id_token,
                    client,
                    expected_nonce.is_none().then_some(received_at),
                )
                .await?;
            if claims.sub.is_empty()
                || claims.sub.len() > 1024
                || expected_subject.is_some_and(|subject| subject != claims.sub)
                || expected_nonce.is_some_and(|nonce| {
                    !claims
                        .nonce
                        .as_ref()
                        .is_some_and(|returned| constant_eq(returned, nonce))
                })
                || (expected_nonce.is_none()
                    && claims.nonce.as_ref().is_some_and(|nonce| {
                        previous.is_some_and(|previous| !constant_eq(nonce, &previous.nonce))
                    }))
            {
                return Err(AuthError::InvalidIdentity);
            }
            (claims.sub, id_token)
        } else if let (Some(previous), Some(subject)) = (previous, expected_subject) {
            (subject.to_owned(), previous.id_token.clone())
        } else {
            return Err(AuthError::InvalidIdentity);
        };
        let tokens = Tokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            id_token,
            scopes: response
                .scope
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            expires_at: received_at.saturating_add(response.expires_in as i64),
            nonce: expected_nonce
                .map(str::to_owned)
                .or_else(|| previous.map(|previous| previous.nonce.clone()))
                .ok_or(AuthError::InvalidIdentity)?,
        };
        Ok((subject, tokens))
    }

    async fn verify_identity(
        &self,
        token: &str,
        client: &str,
        received_at: Option<i64>,
    ) -> Result<Claims> {
        let header = decode_header(token).map_err(|_| AuthError::InvalidIdentity)?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::InvalidIdentity);
        }
        let kid = header.kid.ok_or(AuthError::InvalidIdentity)?;
        if kid.is_empty() || kid.len() > 512 {
            return Err(AuthError::InvalidIdentity);
        }
        let response = self
            .inner
            .client
            .get(self.inner.endpoints.discovery.clone())
            .send()
            .await
            .map_err(|_| AuthError::Connection)?;
        if response.status() != StatusCode::OK {
            return Err(AuthError::Provider);
        }
        let discovery: Discovery = serde_json::from_slice(&bounded_body(response).await?)
            .map_err(|_| AuthError::InvalidIdentity)?;
        let jwks_url = Url::parse(&discovery.jwks_uri).map_err(|_| AuthError::InvalidIdentity)?;
        if discovery.issuer != self.inner.endpoints.issuer
            || !self.inner.endpoints.permits_jwks(&jwks_url)
        {
            return Err(AuthError::InvalidIdentity);
        }
        let response = self
            .inner
            .client
            .get(jwks_url)
            .send()
            .await
            .map_err(|_| AuthError::Connection)?;
        if response.status() != StatusCode::OK {
            return Err(AuthError::Provider);
        }
        let jwks: JwkSet = serde_json::from_slice(&bounded_body(response).await?)
            .map_err(|_| AuthError::InvalidIdentity)?;
        let mut matches = jwks
            .keys
            .iter()
            .filter(|key| key.common.key_id.as_deref() == Some(&kid));
        let key = matches.next().ok_or(AuthError::InvalidIdentity)?;
        if matches.next().is_some()
            || key
                .common
                .key_algorithm
                .is_some_and(|algorithm| algorithm != KeyAlgorithm::RS256)
            || key
                .common
                .public_key_use
                .as_ref()
                .is_some_and(|usage| *usage != PublicKeyUse::Signature)
            || key
                .common
                .key_operations
                .as_ref()
                .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
        {
            return Err(AuthError::InvalidIdentity);
        }
        let key = DecodingKey::from_jwk(key).map_err(|_| AuthError::InvalidIdentity)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[client]);
        validation.set_issuer(&[self.inner.endpoints.issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 0;
        if received_at.is_some() {
            // A pending refresh response was received over the fixed trusted
            // token endpoint. Verify its ID token at that recorded receipt time
            // so a validation outage cannot destroy a still-renewable session.
            // Signature, issuer, audience, subject and nonce checks stay active.
            validation.validate_exp = false;
            validation.validate_nbf = false;
        }
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidIdentity)?
            .claims;
        if received_at.is_some_and(|received_at| {
            claims.exp <= received_at || claims.nbf.is_some_and(|nbf| nbf > received_at)
        }) {
            return Err(AuthError::InvalidIdentity);
        }
        if claims.azp.as_ref().is_some_and(|party| party != client)
            || (claims
                .aud
                .as_array()
                .is_some_and(|audiences| audiences.len() > 1)
                && claims.azp.as_deref() != Some(client))
        {
            return Err(AuthError::InvalidIdentity);
        }
        Ok(claims)
    }
}

fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Connection)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn constant_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64 * 1024
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BODY as u64)
    {
        return Err(AuthError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AuthError::Connection)?;
        if bytes.len() + chunk.len() > MAX_BODY {
            return Err(AuthError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
