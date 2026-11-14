//! The stylesheet and the script, embedded in the binary.

use axum::http::header;
use axum::response::{IntoResponse, Response};

/// Where the stylesheet is served.
pub(crate) const CSS_PATH: &str = "/admin/static/admin.css";
/// Where the script is served.
pub(crate) const JS_PATH: &str = "/admin/static/admin.js";

const CSS: &str = include_str!("admin.css");
const JS: &str = include_str!("admin.js");

pub(crate) async fn css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=3600"),
        ],
        CSS,
    )
        .into_response()
}

pub(crate) async fn js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "max-age=3600"),
        ],
        JS,
    )
        .into_response()
}
