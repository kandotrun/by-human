//! Browser sign-in sessions. The browser keeps a random secret in an
//! `HttpOnly` cookie and PostgreSQL keeps only its SHA-256 hash, so the server
//! token itself is never stored by a browser.
use std::net::SocketAddr;

use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub const COOKIE_NAME: &str = "by_human_session";
/// A session ends after 30 days without use.
pub const IDLE_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;
/// The cookie is only a handle; the server decides when it expires. 400 days
/// is the longest lifetime browsers keep.
const COOKIE_MAX_AGE_SECS: i64 = 400 * 24 * 60 * 60;

pub struct SessionSecret([u8; 32]);

impl SessionSecret {
    pub fn generate() -> Option<Self> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).ok()?;
        Some(Self(bytes))
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn decode(value: &str) -> Option<Self> {
        if value.len() != 43 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0).into()
    }
}

/// Identifies the server token a session was issued under. Rotating the token
/// therefore ends every browser session.
pub fn token_fingerprint(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub enum CookieSecret {
    Missing,
    Invalid,
    Present(SessionSecret),
}

/// Accepts exactly one well-formed session cookie across all `Cookie` headers.
pub fn read_cookie(headers: &HeaderMap) -> CookieSecret {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            return CookieSecret::Invalid;
        };
        for pair in value.split(';') {
            let Some((name, secret)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != COOKIE_NAME {
                continue;
            }
            if found.replace(secret).is_some() {
                return CookieSecret::Invalid;
            }
        }
    }
    match found {
        None => CookieSecret::Missing,
        Some(value) => {
            SessionSecret::decode(value).map_or(CookieSecret::Invalid, CookieSecret::Present)
        }
    }
}

/// `secure` adds the `Secure` attribute for a page served over HTTPS.
pub fn session_cookie(secret: &SessionSecret, secure: bool) -> HeaderValue {
    cookie(&secret.encode(), COOKIE_MAX_AGE_SECS, secure)
}

pub fn expired_cookie(secure: bool) -> HeaderValue {
    cookie("", 0, secure)
}

fn cookie(value: &str, max_age: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .expect("session cookies contain only base64url header text")
}

/// Exact page origins that may create and use browser sessions.
#[derive(Clone, Default)]
pub struct BrowserOrigins(Vec<String>);

impl BrowserOrigins {
    /// Pages served directly by a loopback listener. Other listeners get none.
    pub fn loopback(addr: SocketAddr) -> Self {
        if !addr.ip().is_loopback() {
            return Self::default();
        }
        // Browsers omit the default port from an HTTP origin.
        let port = match addr.port() {
            80 => String::new(),
            port => format!(":{port}"),
        };
        let direct = match addr {
            SocketAddr::V4(v4) => format!("http://{}{port}", v4.ip()),
            SocketAddr::V6(v6) => format!("http://[{}]{port}", v6.ip()),
        };
        Self(vec![direct, format!("http://localhost{port}")])
    }

    /// Adds an HTTPS origin that proxies to the loopback listener, such as
    /// Tailscale Serve. A listener without loopback origins gains nothing.
    pub fn with_public(mut self, origin: Option<String>) -> Self {
        if let Some(origin) = origin
            && !self.0.is_empty()
        {
            self.0.push(origin);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when the single allowed `Origin` is HTTPS, so cookies set for it
    /// must be `Secure`. Loopback pages over HTTP keep cookies without it.
    pub fn secure(&self, headers: &HeaderMap) -> bool {
        self.allows(headers)
            && headers
                .get(header::ORIGIN)
                .and_then(|origin| origin.to_str().ok())
                .is_some_and(|origin| origin.starts_with("https://"))
    }

    /// True only for a single `Origin` header that exactly matches.
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::ORIGIN).iter();
        let (Some(origin), None) = (values.next(), values.next()) else {
            return false;
        };
        origin
            .to_str()
            .is_ok_and(|origin| self.0.iter().any(|allowed| allowed == origin))
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::{HeaderMap, HeaderValue, header};

    use super::*;

    fn cookies(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn secrets_round_trip_as_43_base64url_characters() {
        let secret = SessionSecret::generate().unwrap();
        let encoded = secret.encode();
        assert_eq!(encoded.len(), 43);
        let CookieSecret::Present(parsed) =
            read_cookie(&cookies(&[&format!("{COOKIE_NAME}={encoded}")]))
        else {
            panic!("cookie was not read");
        };
        assert_eq!(parsed.hash(), secret.hash());
        assert_ne!(secret.hash(), token_fingerprint(&encoded));
    }

    #[test]
    fn exactly_one_well_formed_session_cookie_is_accepted() {
        let value = "A".repeat(43);
        assert!(matches!(
            read_cookie(&HeaderMap::new()),
            CookieSecret::Missing
        ));
        assert!(matches!(
            read_cookie(&cookies(&["theme=dark"])),
            CookieSecret::Missing
        ));
        assert!(matches!(
            read_cookie(&cookies(&[&format!("by_human_session_x={value}")])),
            CookieSecret::Missing
        ));
        assert!(matches!(
            read_cookie(&cookies(&[&format!("theme=dark; {COOKIE_NAME}={value}")])),
            CookieSecret::Present(_)
        ));
        assert!(matches!(
            read_cookie(&cookies(&[&format!(
                "{COOKIE_NAME}={value}; {COOKIE_NAME}={value}"
            )])),
            CookieSecret::Invalid
        ));
        assert!(matches!(
            read_cookie(&cookies(&[
                &format!("{COOKIE_NAME}={value}"),
                &format!("{COOKIE_NAME}={value}")
            ])),
            CookieSecret::Invalid
        ));
        assert!(matches!(
            read_cookie(&cookies(&[&format!("{COOKIE_NAME}=short")])),
            CookieSecret::Invalid
        ));
        assert!(matches!(
            read_cookie(&cookies(&[&format!("{COOKIE_NAME}={}=", "A".repeat(42))])),
            CookieSecret::Invalid
        ));
    }

    #[test]
    fn cookies_are_http_only_strict_and_long_lived_handles() {
        let secret = SessionSecret::generate().unwrap();
        assert_eq!(
            session_cookie(&secret, false).to_str().unwrap(),
            format!(
                "{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=34560000",
                secret.encode()
            )
        );
        assert_eq!(
            expired_cookie(false).to_str().unwrap(),
            format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
        );
        assert!(
            session_cookie(&secret, true)
                .to_str()
                .unwrap()
                .ends_with("; Max-Age=34560000; Secure")
        );
        assert!(
            expired_cookie(true)
                .to_str()
                .unwrap()
                .ends_with("; Max-Age=0; Secure")
        );
    }

    #[test]
    fn a_public_https_origin_joins_only_a_loopback_listener() {
        let origin = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, HeaderValue::from_static(value));
            headers
        };
        let public = Some("https://mac.example.ts.net".to_owned());
        let origins = BrowserOrigins::loopback("127.0.0.1:4310".parse::<SocketAddr>().unwrap())
            .with_public(public.clone());
        assert!(origins.allows(&origin("https://mac.example.ts.net")));
        assert!(origins.allows(&origin("http://127.0.0.1:4310")));
        assert!(!origins.allows(&origin("http://mac.example.ts.net")));
        assert!(origins.secure(&origin("https://mac.example.ts.net")));
        assert!(!origins.secure(&origin("http://127.0.0.1:4310")));
        assert!(
            BrowserOrigins::loopback("0.0.0.0:4310".parse::<SocketAddr>().unwrap())
                .with_public(public)
                .is_empty()
        );
    }

    #[test]
    fn the_default_http_port_is_omitted_like_browsers_do() {
        let origins = BrowserOrigins::loopback("127.0.0.1:80".parse::<SocketAddr>().unwrap());
        let allows = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, HeaderValue::from_static(value));
            origins.allows(&headers)
        };
        assert!(allows("http://127.0.0.1"));
        assert!(allows("http://localhost"));
        assert!(!allows("http://127.0.0.1:80"));
    }

    #[test]
    fn only_exact_loopback_origins_of_the_listener_are_allowed() {
        let origins = BrowserOrigins::loopback("127.0.0.1:4310".parse::<SocketAddr>().unwrap());
        let origin = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(header::ORIGIN, HeaderValue::from_str(value).unwrap());
            }
            origins.allows(&headers)
        };
        assert!(origin(&["http://127.0.0.1:4310"]));
        assert!(origin(&["http://localhost:4310"]));
        assert!(!origin(&[]));
        assert!(!origin(&["null"]));
        assert!(!origin(&["http://127.0.0.1:4311"]));
        assert!(!origin(&["http://127.0.0.1:4310/"]));
        assert!(!origin(&["https://127.0.0.1:4310"]));
        assert!(!origin(&["http://evil.example"]));
        assert!(!origin(&["http://127.0.0.1:4310", "http://127.0.0.1:4310"]));
        let v6 = BrowserOrigins::loopback("[::1]:4310".parse().unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://[::1]:4310"),
        );
        assert!(v6.allows(&headers));
        let public = BrowserOrigins::loopback("192.0.2.1:4310".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://192.0.2.1:4310"),
        );
        assert!(!public.allows(&headers));
    }
}
