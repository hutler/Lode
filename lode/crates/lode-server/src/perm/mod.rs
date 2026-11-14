//! Access rules (SPEC §5.5): evaluation, the in-memory rule cache, and the `/admin/acl`
//! endpoints.

mod eval;
mod handlers;

pub use eval::{BranchAccess, DepotAccess, Explained, Level, Permissions};
pub use handlers::routes;
