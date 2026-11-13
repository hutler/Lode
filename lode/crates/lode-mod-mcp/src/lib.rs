//! Lode module: the server MCP endpoint (ARCHITECTURE §9.3, SPEC §6.4).
//!
//! Serves `/mcp` over the streamable HTTP transport of the official Rust MCP SDK (`rmcp`),
//! authenticated by the same bearer tokens as the HTTP API. The endpoint hosts every tool in
//! the registry (§8), listing only those whose scope the caller's token carries. This module
//! adds the tools that read core data ([`tools`]); the locks and shelves modules add theirs.
//!
//! The endpoint is stateless: each request carries its token and is answered on its own, so
//! nothing is kept between requests and any server instance can answer any request.

#![forbid(unsafe_code)]

mod tools;

use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use lode_proto::commit::Via;
use lode_server::module::{Module, ModuleCtx, ToolDef};
use lode_server::{ApiError, Caller};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{json, Value};

/// Module name, as in `[modules]`.
pub const NAME: &str = "mcp";

/// Path of the endpoint.
pub const PATH: &str = "/mcp";

/// Largest request body the endpoint accepts. Tool inputs that carry file content (a shelf
/// made through MCP) are capped well below this by their own limits.
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

const INSTRUCTIONS: &str = "Lode is a version control system. A server holds depots; a depot \
holds branches; every commit makes a numbered revision that is unique within its depot. Paths \
are relative, use `/`, and never start with `/`. You only ever see what your token may read: \
anything else looks like it does not exist. Tool errors carry a stable `code`, such as \
`not_found`, `permission_denied` or `invalid_request`.";

/// The MCP module.
#[derive(Debug, Default)]
pub struct Mcp;

impl Mcp {
    /// The module, ready to register.
    pub fn module() -> Arc<dyn Module> {
        Arc::new(Self)
    }
}

impl Module for Mcp {
    fn name(&self) -> &'static str {
        NAME
    }

    fn routes(&self, ctx: ModuleCtx) -> Option<Router> {
        let server = ctx.clone();
        // Bearer tokens cannot be sent by a page in someone's browser, so the DNS-rebinding
        // guard (a `Host` allow-list that defaults to loopback) is not needed here and would
        // break every deployment behind a real host name.
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_max_request_body_bytes(MAX_REQUEST_BYTES)
            .disable_allowed_hosts();
        let service = StreamableHttpService::new(
            move || {
                Ok(Server {
                    ctx: server.clone(),
                })
            },
            Arc::new(NeverSessionManager::default()),
            config,
        );
        Some(
            Router::new()
                .route_service(PATH, service)
                .layer(middleware::from_fn_with_state(ctx, authenticate)),
        )
    }

    fn mcp_tools(&self, ctx: ModuleCtx) -> Vec<ToolDef> {
        tools::core_tools(&ctx)
    }
}

/// Resolves the bearer token before the MCP transport sees the request, so an
/// unauthenticated request gets the standard 401 with a `WWW-Authenticate` challenge.
async fn authenticate(State(ctx): State<ModuleCtx>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    match <Caller as FromRequestParts<ModuleCtx>>::from_request_parts(&mut parts, &ctx).await {
        Ok(mut caller) => {
            caller.via = Via::Mcp;
            parts.extensions.insert(caller);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(e) => {
            let mut resp = e.into_response();
            if resp.status() == StatusCode::UNAUTHORIZED {
                resp.headers_mut().insert(
                    WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"lode\""),
                );
            }
            resp
        }
    }
}

/// One MCP server instance; the transport makes one per request.
#[derive(Clone)]
struct Server {
    ctx: ModuleCtx,
}

/// The caller the middleware put on the HTTP request.
fn caller(rc: &RequestContext<RoleServer>) -> Result<Caller, ErrorData> {
    rc.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|p| p.extensions.get::<Caller>())
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("the request carries no caller", None))
}

fn mcp_tool(t: &ToolDef) -> Tool {
    let schema = match &t.input_schema {
        Value::Object(o) => o.clone(),
        _ => serde_json::Map::new(),
    };
    Tool::new(t.name.clone(), t.description.clone(), Arc::new(schema))
}

/// A Lode error as an MCP tool error: the model sees the code and can act on it.
fn tool_error(e: &ApiError) -> CallToolResult {
    let body = e.body();
    let mut v = json!({ "code": body.code, "message": body.message });
    if !body.details.is_null() {
        v["details"] = body.details.clone();
    }
    CallToolResult::structured_error(v)
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("lode", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        rc: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let caller = caller(&rc)?;
        let tools = self
            .ctx
            .tools()
            .all()
            .filter(|t| caller.has_scope(&t.scope))
            .map(mcp_tool)
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.ctx.tools().get(name).map(mcp_tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caller = caller(&rc)?;
        let Some(tool) = self.ctx.tools().get(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            ));
        };
        if let Err(e) = caller.require_scope(&tool.scope) {
            return Ok(tool_error(&e).into());
        }
        let input = Value::Object(request.arguments.unwrap_or_default());
        let result = match (tool.handler)(caller, input).await {
            Ok(v) => CallToolResult::structured(v),
            Err(e) => tool_error(&e),
        };
        Ok(result.into())
    }
}
