//! Router assembly: core routes, the error fallback and the middleware stack.

use std::any::Any;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderName, Request, Response};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use lode_proto::auth::{Info, SsoInfo};
use lode_proto::consts::{paths, API_VERSION};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::error::ApiError;
use crate::module::ModuleCtx;
use crate::state::AppState;

/// Header carrying the per-request id.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// The core routes, without fallback or middleware. Modules and tests merge their own routes
/// into this before calling [`finish`].
pub fn core_routes() -> Router<AppState> {
    Router::new()
        .route(paths::INFO, get(info))
        .merge(crate::auth::routes())
        .merge(crate::perm::routes())
        .merge(crate::depots::routes())
        .merge(crate::blobs::routes())
        .merge(crate::commit::routes())
        .merge(crate::read::routes())
        .merge(crate::branches::routes())
        .merge(crate::maintenance::routes())
        .merge(crate::trash::routes())
}

/// Binds the state, mounts the routes of the enabled modules, and adds the error fallback
/// and the middleware stack.
pub fn finish(routes: Router<AppState>, state: AppState) -> Router {
    let limit = usize::try_from(state.config().limits.max_request_json).unwrap_or(usize::MAX);
    let request_id = HeaderName::from_static(REQUEST_ID_HEADER);
    let mut app: Router = routes.with_state(state.clone());
    for m in state.modules() {
        if let Some(r) = m.routes(ModuleCtx::new(state.clone(), m.name())) {
            app = app.merge(r);
        }
    }
    // The admin web UI sends its actions to the API router built so far.
    let app = app.clone().merge(crate::admin::routes(state.clone(), app));
    app.fallback(fallback)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(limit))
        .layer(axum::middleware::map_request_with_state(
            state.clone(),
            crate::idle::limit_idle,
        ))
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
                .layer(TraceLayer::new_for_http().make_span_with(make_span))
                .layer(PropagateRequestIdLayer::new(request_id))
                .layer(CatchPanicLayer::custom(on_panic)),
        )
}

/// The full router: core routes plus those of the enabled modules.
pub fn router(state: AppState) -> Router {
    finish(core_routes(), state)
}

async fn info(State(state): State<AppState>) -> Json<Info> {
    Json(Info {
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: API_VERSION.to_owned(),
        modules: state.module_names().to_vec(),
        sso: state.config().oidc.as_ref().map(|o| SsoInfo {
            label: o.label.clone(),
            password_login: o.password_login == crate::config::PasswordLogin::Everyone,
        }),
        chunking: Some(lode_proto::auth::ChunkingInfo::current()),
        proxy: None,
    })
}

async fn fallback() -> ApiError {
    ApiError::not_found("no such endpoint")
}

async fn method_not_allowed() -> ApiError {
    ApiError::not_found("this endpoint does not support that method")
}

fn make_span<B>(req: &Request<B>) -> tracing::Span {
    let id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    // The path is logged, the query string is not: it can carry depot paths.
    tracing::info_span!("request", request_id = %id, method = %req.method(), path = %req.uri().path())
}

fn on_panic(payload: Box<dyn Any + Send + 'static>) -> Response<axum::body::Body> {
    // The message goes to the server log only; the client gets a fixed `internal` body.
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    tracing::error!(panic = %message, "request handler panicked");
    ApiError::internal().into_response()
}
