//! Synthetic credentials, fresh test-only signing keys, and literal loopback.

use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use aws_lc_rs::{
    rand::SystemRandom,
    rsa::{KeyPair, KeySize},
    signature::{KeyPair as _, RSA_PKCS1_SHA256},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, jwk::Jwk};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::{AuthError, AuthManager, Endpoints, PendingLogin, SCOPES, now};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("by-human-auth-{}", uuid::Uuid::new_v4())))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        // Only the UUID test directory created by this fixture is removed.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Default)]
struct Settings {
    nonce: String,
    subject: String,
    audience: String,
    missing_scope: bool,
    expired_identity: bool,
    invalid_signature: bool,
    refresh_error: Option<String>,
    refresh_delay: u64,
    external_jwks: bool,
    validation_outage: Option<String>,
    refresh_lifetime: Option<u64>,
    future_identity: bool,
}

struct Fixture {
    issuer: String,
    key: KeyPair,
    wrong_key: KeyPair,
    jwk: Value,
    settings: Mutex<Settings>,
    requests: Mutex<Vec<HashMap<String, String>>>,
    current_refresh: Mutex<String>,
    count: AtomicUsize,
    refresh_started: tokio::sync::Notify,
}

struct Server {
    fixture: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let key = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let public = DecodingKey::from_rsa_der(key.public_key().as_ref());
        let mut jwk = Jwk::from_decoding_key(&public, Some(Algorithm::RS256)).unwrap();
        jwk.common.key_id = Some("fixture-key".into());
        let fixture = Arc::new(Fixture {
            issuer,
            key,
            wrong_key: KeyPair::generate(KeySize::Rsa2048).unwrap(),
            jwk: serde_json::to_value(jwk).unwrap(),
            settings: Mutex::new(Settings {
                subject: "fixture-subject".into(),
                audience: "oaiapp_fixture".into(),
                ..Default::default()
            }),
            requests: Mutex::new(Vec::new()),
            current_refresh: Mutex::new(String::new()),
            count: AtomicUsize::new(0),
            refresh_started: tokio::sync::Notify::new(),
        });
        let app = Router::new()
            .route("/token", post(token))
            .route("/discovery", get(discovery))
            .route("/jwks", get(jwks))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { fixture, task }
    }

    fn manager(&self, directory: &TestDirectory) -> AuthManager {
        let base = &self.fixture.issuer;
        AuthManager::build(
            &directory.0,
            Endpoints {
                authorize: Url::parse(&format!("{base}/authorize")).unwrap(),
                token: Url::parse(&format!("{base}/token")).unwrap(),
                discovery: Url::parse(&format!("{base}/discovery")).unwrap(),
                issuer: base.clone(),
            },
        )
        .unwrap()
    }

    async fn pending(&self, manager: &AuthManager, new_registration: bool) -> PendingLogin {
        let pending = manager.begin_login(new_registration).await.unwrap();
        let url = Url::parse(pending.authorization_url()).unwrap();
        let query: HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        self.fixture.settings.lock().unwrap().nonce = query["nonce"].clone();
        pending
    }

    async fn finish(&self, pending: PendingLogin) -> super::Result<super::AuthStatus> {
        let callback = callback_url(&pending, Some("oaiapp_fixture"));
        let task = tokio::spawn(async move { pending.complete(&CancellationToken::new()).await });
        let response = reqwest::get(callback).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        task.await.unwrap()
    }

    async fn login(&self, manager: &AuthManager) {
        let pending = self.pending(manager, false).await;
        assert!(self.finish(pending).await.unwrap().plan_enabled);
    }
}

fn callback_url(pending: &PendingLogin, client: Option<&str>) -> Url {
    let url = Url::parse(pending.authorization_url()).unwrap();
    let query: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut callback = Url::parse(&query["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &query["state"])
        .append_pair("code", "fixture-code");
    if let Some(client) = client {
        callback.query_pairs_mut().append_pair("client_id", client);
    }
    callback
}

async fn discovery(State(fixture): State<Arc<Fixture>>) -> (StatusCode, Json<Value>) {
    let mut settings = fixture.settings.lock().unwrap();
    if settings.validation_outage.as_deref() == Some("discovery") {
        settings.validation_outage = None;
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let external = settings.external_jwks;
    (
        StatusCode::OK,
        Json(json!({
            "issuer": fixture.issuer,
            "jwks_uri": if external { "https://example.invalid/jwks".to_owned() }
                else { format!("{}/jwks",fixture.issuer) },
        })),
    )
}

async fn jwks(State(fixture): State<Arc<Fixture>>) -> (StatusCode, Json<Value>) {
    let mut settings = fixture.settings.lock().unwrap();
    if settings.validation_outage.as_deref() == Some("jwks") {
        settings.validation_outage = None;
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    (StatusCode::OK, Json(json!({"keys":[fixture.jwk]})))
}

async fn token(State(fixture): State<Arc<Fixture>>, body: Bytes) -> (StatusCode, Json<Value>) {
    let form_url = Url::parse(&format!(
        "http://127.0.0.1/?{}",
        String::from_utf8_lossy(&body)
    ))
    .unwrap();
    let form: HashMap<String, String> = form_url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    fixture.requests.lock().unwrap().push(form.clone());
    let settings = fixture.settings.lock().unwrap().clone();
    if form
        .get("grant_type")
        .is_some_and(|grant| grant == "refresh_token")
    {
        fixture.refresh_started.notify_one();
        tokio::time::sleep(Duration::from_millis(settings.refresh_delay)).await;
        if let Some(error) = settings.refresh_error {
            return (StatusCode::BAD_REQUEST, Json(json!({"error":error})));
        }
        if form.get("refresh_token") != Some(&fixture.current_refresh.lock().unwrap()) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"refresh_token_reused"})),
            );
        }
    }
    let count = fixture.count.fetch_add(1, Ordering::SeqCst) + 1;
    let lifetime = if form
        .get("grant_type")
        .is_some_and(|grant| grant == "refresh_token")
    {
        settings.refresh_lifetime.unwrap_or(3600)
    } else {
        3600
    };
    let refresh = format!("fixture-refresh-{count}");
    *fixture.current_refresh.lock().unwrap() = refresh.clone();
    let mut claims = json!({
        "iss":fixture.issuer,"aud":settings.audience,"sub":settings.subject,
        "exp":if settings.expired_identity {now()-60} else {now()+lifetime as i64},
        "nonce":settings.nonce,
    });
    if settings.future_identity {
        claims["nbf"] = json!(now() + 60);
    }
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"fixture-key","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let mut signature = vec![0; 256];
    let key = if settings.invalid_signature {
        &fixture.wrong_key
    } else {
        &fixture.key
    };
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    let id_token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature));
    (
        StatusCode::OK,
        Json(json!({
            "access_token":format!("fixture-access-{count}"),"refresh_token":refresh,
            "id_token":id_token,"token_type":"Bearer","expires_in":lifetime,
            "scope":if settings.missing_scope {"openid offline_access"} else {SCOPES},
        })),
    )
}

async fn expire(manager: &AuthManager) {
    let _lock = manager.inner.directory.lock().await.unwrap();
    let mut saved = manager.inner.directory.read().unwrap().unwrap();
    saved
        .registration
        .as_mut()
        .unwrap()
        .tokens
        .as_mut()
        .unwrap()
        .expires_at = now() - 1;
    manager.inner.directory.write(&saved).unwrap();
}

#[tokio::test]
async fn registration_uses_pkce_and_persists_only_verified_issued_client() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    let pending = server.pending(&manager, false).await;
    let url = Url::parse(pending.authorization_url()).unwrap();
    let query: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["agent_name_hint"], "by-human");
    assert_eq!(query["resource"], "https://api.openai.com/v1");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query.contains_key("id_token_hint"));
    assert!(directory.0.join("openai.json").exists());
    let host = query["ext_agent_host_id"].clone();
    assert!(host.starts_with("urn:uuid:"));
    assert!(server.finish(pending).await.unwrap().authenticated);
    assert_eq!(
        manager.access_token().await.unwrap().as_str(),
        "fixture-access-1"
    );
    assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(directory.0.join("openai.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    let request = server.fixture.requests.lock().unwrap()[0].clone();
    assert_eq!(request["client_id"], "oaiapp_fixture");
    assert_eq!(
        URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(request["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
    let pending = server.pending(&manager, false).await;
    let url = Url::parse(pending.authorization_url()).unwrap();
    let query: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["client_id"], "oaiapp_fixture");
    assert_eq!(query["ext_agent_host_id"], host);
    assert!(!query.contains_key("agent_name_hint"));
}

#[tokio::test]
async fn bad_state_never_exchanges_and_valid_callback_can_still_complete() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    let pending = server.pending(&manager, false).await;
    let valid = callback_url(&pending, Some("oaiapp_fixture"));
    let mut invalid = valid.clone();
    invalid.set_query(Some(
        "state=wrong&code=fixture-code&client_id=oaiapp_fixture",
    ));
    let task = tokio::spawn(async move { pending.complete(&CancellationToken::new()).await });
    assert_eq!(
        reqwest::get(invalid).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert!(server.fixture.requests.lock().unwrap().is_empty());
    assert_eq!(reqwest::get(valid).await.unwrap().status(), StatusCode::OK);
    assert!(task.await.unwrap().unwrap().plan_enabled);
}

#[tokio::test]
async fn registration_requires_issued_client_and_returning_client_cannot_change() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    for returned in [None, Some("dynamic_agent_client")] {
        let pending = server.pending(&manager, false).await;
        let callback = callback_url(&pending, returned);
        let task = tokio::spawn(async move { pending.complete(&CancellationToken::new()).await });
        reqwest::get(callback).await.unwrap();
        assert_eq!(task.await.unwrap(), Err(AuthError::InvalidClient));
    }
    assert!(server.fixture.requests.lock().unwrap().is_empty());
    server.login(&manager).await;
    let pending = server.pending(&manager, false).await;
    let callback = callback_url(&pending, Some("oaiapp_other"));
    let task = tokio::spawn(async move { pending.complete(&CancellationToken::new()).await });
    reqwest::get(callback).await.unwrap();
    assert_eq!(task.await.unwrap(), Err(AuthError::InvalidClient));
    assert_eq!(server.fixture.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn nonce_audience_expiry_and_signature_are_verified_before_saving() {
    let server = Server::new().await;
    for case in ["nonce", "audience", "expiry", "signature", "jwks"] {
        let directory = TestDirectory::new();
        let manager = server.manager(&directory);
        let pending = server.pending(&manager, false).await;
        {
            let mut settings = server.fixture.settings.lock().unwrap();
            settings.audience = if case == "audience" {
                "wrong-client"
            } else {
                "oaiapp_fixture"
            }
            .into();
            if case == "nonce" {
                settings.nonce = "wrong-nonce".into();
            }
            settings.expired_identity = case == "expiry";
            settings.invalid_signature = case == "signature";
            settings.external_jwks = case == "jwks";
        }
        assert_eq!(
            server.finish(pending).await,
            Err(AuthError::InvalidIdentity),
            "{case}"
        );
        assert!(!manager.status().await.unwrap().registration_saved);
    }
}

#[tokio::test]
async fn missing_plan_scope_retains_identity_without_permitting_inference() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.fixture.settings.lock().unwrap().missing_scope = true;
    let pending = server.pending(&manager, false).await;
    let status = server.finish(pending).await.unwrap();
    assert!(status.authenticated);
    assert!(!status.plan_enabled);
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::MissingPermission)
    ));
}

#[tokio::test]
async fn explicit_plan_consent_enables_usage_without_replacing_registration() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.fixture.settings.lock().unwrap().missing_scope = true;
    let pending = server.pending(&manager, false).await;
    assert!(!server.finish(pending).await.unwrap().plan_enabled);

    let pending = manager.begin_plan_consent().await.unwrap();
    let query: HashMap<_, _> = Url::parse(pending.authorization_url())
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["client_id"], "oaiapp_fixture");
    assert_eq!(query["prompt"], "consent");
    assert_eq!(query["scope"], SCOPES);
    assert!(!query.contains_key("agent_name_hint"));
    {
        let mut settings = server.fixture.settings.lock().unwrap();
        settings.nonce = query["nonce"].clone();
        settings.missing_scope = false;
    }
    assert!(server.finish(pending).await.unwrap().plan_enabled);
    assert_eq!(
        manager.access_token().await.unwrap().as_str(),
        "fixture-access-2"
    );
    let ordinary = server.pending(&manager, false).await;
    let ordinary = Url::parse(ordinary.authorization_url()).unwrap();
    assert!(!ordinary.query_pairs().any(|(key, _)| key == "prompt"));
}

#[tokio::test]
async fn refresh_is_serialized_across_manager_instances_and_rotated_pair_is_saved() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let first = server.manager(&directory);
    server.login(&first).await;
    expire(&first).await;
    server.fixture.settings.lock().unwrap().refresh_delay = 80;
    let second = server.manager(&directory);
    let (one, two) = tokio::join!(first.access_token(), second.access_token());
    assert_eq!(one.unwrap().as_str(), "fixture-access-2");
    assert_eq!(two.unwrap().as_str(), "fixture-access-2");
    assert_eq!(server.fixture.count.load(Ordering::SeqCst), 2);
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests[1]["grant_type"], "refresh_token");
    assert_eq!(requests[1]["refresh_token"], "fixture-refresh-1");
    assert_eq!(requests[1]["client_id"], "oaiapp_fixture");
    assert_eq!(requests[1]["resource"], "https://api.openai.com/v1");
    assert!(!requests[1].contains_key("scope"));
    drop(requests);
    let saved = first.inner.directory.read().unwrap().unwrap();
    let tokens = saved.registration.unwrap().tokens.unwrap();
    assert_eq!(tokens.refresh_token, "fixture-refresh-2");
    assert_eq!(tokens.access_token, "fixture-access-2");
}

#[tokio::test]
async fn rotated_refresh_survives_validation_outage_and_restart_without_token_reuse() {
    for endpoint in ["discovery", "jwks"] {
        let server = Server::new().await;
        let directory = TestDirectory::new();
        let manager = server.manager(&directory);
        server.login(&manager).await;
        expire(&manager).await;
        server.fixture.settings.lock().unwrap().validation_outage = Some(endpoint.into());
        assert!(matches!(
            manager.access_token().await,
            Err(AuthError::Provider)
        ));
        assert_eq!(server.fixture.count.load(Ordering::SeqCst), 2);

        // Simulate delayed retry without rebasing the access-token lifetime.
        let received_at = now() - 120;
        {
            let _lock = manager.inner.directory.lock().await.unwrap();
            let mut saved = manager.inner.directory.read().unwrap().unwrap();
            saved
                .registration
                .as_mut()
                .unwrap()
                .pending_refresh
                .as_mut()
                .unwrap()
                .received_at = received_at;
            manager.inner.directory.write(&saved).unwrap();
        }

        // Reopening proves recovery comes from disk, not an in-memory response.
        let reopened = server.manager(&directory);
        assert_eq!(
            reopened.access_token().await.unwrap().as_str(),
            "fixture-access-2"
        );
        let saved = reopened.inner.directory.read().unwrap().unwrap();
        let registration = saved.registration.unwrap();
        assert!(registration.pending_refresh.is_none());
        assert_eq!(registration.tokens.unwrap().expires_at, received_at + 3600);
        assert_eq!(
            server.fixture.requests.lock().unwrap().len(),
            2,
            "validation must not resubmit the superseded refresh token: {endpoint}"
        );
        expire(&reopened).await;
        assert_eq!(
            reopened.access_token().await.unwrap().as_str(),
            "fixture-access-3"
        );
        let requests = server.fixture.requests.lock().unwrap();
        assert_eq!(requests[2]["refresh_token"], "fixture-refresh-2");
    }
}

#[tokio::test]
async fn pending_refresh_never_bypasses_selected_identity_validation() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.login(&manager).await;
    expire(&manager).await;
    server.fixture.settings.lock().unwrap().subject = "fixture-other-subject".into();
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::InvalidIdentity)
    ));

    // A new good provider response must not overwrite the quarantined response
    // by exchanging the superseded token. Explicit sign-in can replace it.
    server.fixture.settings.lock().unwrap().subject = "fixture-subject".into();
    let reopened = server.manager(&directory);
    assert!(matches!(
        reopened.access_token().await,
        Err(AuthError::InvalidIdentity)
    ));
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
    assert!(
        reopened
            .inner
            .directory
            .read()
            .unwrap()
            .unwrap()
            .registration
            .unwrap()
            .pending_refresh
            .is_some()
    );
}

#[tokio::test]
async fn delayed_pending_validation_can_renew_after_the_identity_token_expires() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.login(&manager).await;
    expire(&manager).await;
    {
        let mut settings = server.fixture.settings.lock().unwrap();
        settings.validation_outage = Some("jwks".into());
        settings.refresh_lifetime = Some(1);
    }
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::Provider)
    ));
    server.fixture.settings.lock().unwrap().refresh_lifetime = None;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let reopened = server.manager(&directory);
    assert_eq!(
        reopened.access_token().await.unwrap().as_str(),
        "fixture-access-3"
    );
    let requests = server.fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["refresh_token"], "fixture-refresh-2");
}

#[tokio::test]
async fn pending_refresh_rejects_identity_not_valid_when_received() {
    for future_identity in [false, true] {
        let server = Server::new().await;
        let directory = TestDirectory::new();
        let manager = server.manager(&directory);
        server.login(&manager).await;
        expire(&manager).await;
        {
            let mut settings = server.fixture.settings.lock().unwrap();
            settings.expired_identity = !future_identity;
            settings.future_identity = future_identity;
        }
        assert!(matches!(
            manager.access_token().await,
            Err(AuthError::InvalidIdentity)
        ));
        assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn cancellation_cannot_discard_rotated_tokens_and_shutdown_waits_for_save() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.login(&manager).await;
    expire(&manager).await;
    server.fixture.settings.lock().unwrap().refresh_delay = 150;
    let caller = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.access_token().await })
    };
    server.fixture.refresh_started.notified().await;
    caller.abort();
    manager.settle_refreshes().await.unwrap();
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::Cancelled)
    ));
    let reopened = server.manager(&directory);
    assert_eq!(
        reopened.access_token().await.unwrap().as_str(),
        "fixture-access-2"
    );
    assert_eq!(server.fixture.count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn terminal_refresh_clears_tokens_but_temporary_failure_preserves_them() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.login(&manager).await;
    expire(&manager).await;
    server.fixture.settings.lock().unwrap().refresh_error = Some("temporarily_unavailable".into());
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::Provider)
    ));
    assert!(manager.status().await.unwrap().authenticated);
    server.fixture.settings.lock().unwrap().refresh_error = Some("invalid_grant".into());
    assert!(matches!(
        manager.access_token().await,
        Err(AuthError::Reauthorize)
    ));
    let status = manager.status().await.unwrap();
    assert!(status.registration_saved);
    assert!(!status.authenticated);
}

#[tokio::test]
async fn returning_identity_must_match_and_new_registration_is_explicit() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    server.login(&manager).await;
    server.fixture.settings.lock().unwrap().subject = "fixture-other-subject".into();
    let pending = server.pending(&manager, false).await;
    assert_eq!(
        server.finish(pending).await,
        Err(AuthError::InvalidIdentity)
    );
    assert_eq!(
        manager.access_token().await.unwrap().as_str(),
        "fixture-access-1"
    );
    let pending = server.pending(&manager, true).await;
    assert!(pending.authorization_url().contains("dynamic_agent_client"));
    assert!(server.finish(pending).await.unwrap().plan_enabled);
}

#[tokio::test]
async fn owner_only_permissions_and_symlink_boundaries_are_enforced() {
    let directory = TestDirectory::new();
    fs::create_dir(&directory.0).unwrap();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        AuthManager::new(&directory.0),
        Err(AuthError::UnsafeStorage)
    ));
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    let manager = AuthManager::new(&directory.0).unwrap();
    let outside = TestDirectory::new();
    fs::create_dir(&outside.0).unwrap();
    let target = outside.0.join("untouched.json");
    fs::write(&target, "synthetic untouched marker").unwrap();
    symlink(&target, directory.0.join("openai.json")).unwrap();
    assert!(manager.status().await.is_err());
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "synthetic untouched marker"
    );
    let link = outside.0.join("root-link");
    symlink(&directory.0, &link).unwrap();
    assert!(matches!(
        AuthManager::new(&link),
        Err(AuthError::UnsafeStorage)
    ));
}

#[tokio::test]
async fn cancellation_closes_listener_without_contacting_token_endpoint() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    let pending = server.pending(&manager, false).await;
    let callback = callback_url(&pending, Some("oaiapp_fixture"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(pending.complete(&cancel).await, Err(AuthError::Cancelled));
    assert!(reqwest::get(callback).await.is_err());
    assert!(server.fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn callback_timeout_closes_listener_without_contacting_token_endpoint() {
    let server = Server::new().await;
    let directory = TestDirectory::new();
    let manager = server.manager(&directory);
    let pending = server.pending(&manager, false).await;
    let callback = callback_url(&pending, Some("oaiapp_fixture"));
    assert_eq!(
        pending
            .complete_with_timeout(&CancellationToken::new(), Duration::from_millis(20))
            .await,
        Err(AuthError::Timeout)
    );
    assert!(reqwest::get(callback).await.is_err());
    assert!(server.fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_or_nonregular_storage_never_becomes_an_empty_registration() {
    let directory = TestDirectory::new();
    let manager = AuthManager::new(&directory.0).unwrap();
    let file = directory.0.join("openai.json");
    fs::write(&file, b"broken-json").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(manager.status().await, Err(AuthError::Storage));
    // This fixture alone owns this generated path.
    fs::remove_file(&file).unwrap();
    let name = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: name is a valid NUL-terminated path in this generated test root.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(manager.status().await, Err(AuthError::UnsafeStorage));
}
