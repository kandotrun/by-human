//! The small same-origin client is embedded in the server binary. It contains
//! no credentials and needs no separate build step or application server.
use axum::{
    Router,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::api::ApiState;

pub fn router() -> Router<ApiState> {
    Router::new()
        .route("/", get(index))
        .route("/chat.css", get(styles))
        .route("/chat.js", get(script))
        .route("/markdown.js", get(markdown))
        .route("/manifest.webmanifest", get(manifest))
        .route(
            "/icon-192.png",
            get(|| icon(include_bytes!("../web/icons/icon-192.png"))),
        )
        .route(
            "/icon-512.png",
            get(|| icon(include_bytes!("../web/icons/icon-512.png"))),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| icon(include_bytes!("../web/icons/apple-touch-icon.png"))),
        )
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    bytes(content_type, body.as_bytes())
}

fn bytes(content_type: &'static str, body: &'static [u8]) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

async fn index() -> Response {
    asset(
        "text/html; charset=utf-8",
        include_str!("../web/index.html"),
    )
}

async fn styles() -> Response {
    asset("text/css; charset=utf-8", include_str!("../web/chat.css"))
}

async fn script() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../web/chat.js"),
    )
}

// Home-screen installation on iPhone and other browsers.
async fn manifest() -> Response {
    asset(
        "application/manifest+json",
        include_str!("../web/manifest.webmanifest"),
    )
}

async fn icon(body: &'static [u8]) -> Response {
    bytes("image/png", body)
}

async fn markdown() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../web/markdown.js"),
    )
}
