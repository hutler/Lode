//! Paths, header names, limits and the API version.

/// API version reported by `/info`. Raised on incompatible changes.
pub const API_VERSION: &str = "1";

/// Prefix of every API path.
pub const API_BASE: &str = "/api/v1";

/// Header carrying the access epoch on manifest responses (ARCHITECTURE §6).
pub const HEADER_ACCESS_EPOCH: &str = "X-Lode-Access-Epoch";

/// Header naming the blob id of a file served by `GET /files`.
pub const HEADER_ID: &str = "x-lode-id";

/// Header naming the kind (`file` or `symlink`) of a file served by `GET /files`.
pub const HEADER_KIND: &str = "x-lode-kind";

/// Header saying whether a file served by `GET /files` is executable (`true` or `false`).
pub const HEADER_EXECUTABLE: &str = "x-lode-executable";

/// Media type of manifest responses: one JSON object per line.
pub const NDJSON: &str = "application/x-ndjson";

/// Content encoding for compressed blob transfer.
pub const ENCODING_ZSTD: &str = "zstd";

/// Default limit on a JSON request body.
pub const MAX_REQUEST_JSON: u64 = 64 * 1024 * 1024;

/// Default limit on one blob.
pub const MAX_BLOB_SIZE: u64 = 64 * 1024 * 1024 * 1024;

/// Default limit on changes in one commit.
pub const MAX_CHANGES_PER_COMMIT: usize = 500_000;

/// Default cap on changed paths listed per revision in a revision list.
pub const MAX_CHANGES_LISTED: usize = 1_000;

/// Default page size of the revision list.
pub const DEFAULT_REVISION_PAGE: u32 = 100;

/// Largest page size of the revision list.
pub const MAX_REVISION_PAGE: u32 = 1_000;

/// Most ids in one `objects/missing` call.
pub const MAX_MISSING_QUERY: usize = 10_000;

/// Most provenance bytes stored with a revision or shelf.
pub const MAX_PROVENANCE_BYTES: usize = 4 * 1024;

/// API paths. `{d}`, `{b}`, `{id}` and `{n}` are path parameters.
pub mod paths {
    /// `GET` server information.
    pub const INFO: &str = "/api/v1/info";
    /// `POST` password login.
    pub const LOGIN: &str = "/api/v1/auth/login";
    /// `GET` identity of the caller.
    pub const WHOAMI: &str = "/api/v1/auth/whoami";
    /// `GET` and `POST` own tokens.
    pub const TOKENS: &str = "/api/v1/tokens";
    /// `DELETE` one own token.
    pub const TOKEN: &str = "/api/v1/tokens/{id}";
    /// `GET` and `POST` depots.
    pub const DEPOTS: &str = "/api/v1/depots";
    /// `GET` and `POST` branches.
    pub const BRANCHES: &str = "/api/v1/depots/{d}/branches";
    /// `GET` and `PATCH` one branch.
    pub const BRANCH: &str = "/api/v1/depots/{d}/branches/{b}";
    /// `GET` merge base and recorded merges from `?source=` (SPEC §5.4).
    pub const MERGE_INFO: &str = "/api/v1/depots/{d}/branches/{b}/merge-info";
    /// `POST` which of these objects are missing.
    pub const OBJECTS_MISSING: &str = "/api/v1/depots/{d}/objects/missing";
    /// `PUT` and `GET` a blob.
    pub const BLOB: &str = "/api/v1/depots/{d}/blobs/{id}";
    /// `POST` a commit.
    pub const COMMITS: &str = "/api/v1/depots/{d}/branches/{b}/commits";
    /// `GET` revision list.
    pub const REVISIONS: &str = "/api/v1/depots/{d}/revisions";
    /// `GET` one revision.
    pub const REVISION: &str = "/api/v1/depots/{d}/revisions/{n}";
    /// `GET` manifest (NDJSON).
    pub const MANIFEST: &str = "/api/v1/depots/{d}/manifest";
    /// `GET` one directory.
    pub const TREE: &str = "/api/v1/depots/{d}/tree";
    /// `GET` file content.
    pub const FILES: &str = "/api/v1/depots/{d}/files";
    /// `GET` blame.
    pub const BLAME: &str = "/api/v1/depots/{d}/blame";
    /// Admin: principals.
    pub const ADMIN_PRINCIPALS: &str = "/api/v1/admin/principals";
    /// Admin: one principal (`PATCH`).
    pub const ADMIN_PRINCIPAL: &str = "/api/v1/admin/principals/{name}";
    /// Admin: create a token for another principal (`POST`).
    pub const ADMIN_PRINCIPAL_TOKENS: &str = "/api/v1/admin/principals/{name}/tokens";
    /// One token of a principal (administrators, or the owner of an agent).
    pub const ADMIN_PRINCIPAL_TOKEN: &str = "/api/v1/admin/principals/{name}/tokens/{id}";
    /// Admin: groups.
    pub const ADMIN_GROUPS: &str = "/api/v1/admin/groups";
    /// Admin: add (`PUT`) or remove (`DELETE`) a group member.
    pub const ADMIN_GROUP_MEMBER: &str = "/api/v1/admin/groups/{group}/members/{name}";
    /// Admin: access rules (`GET`, `POST`).
    pub const ADMIN_ACL: &str = "/api/v1/admin/acl";
    /// Admin: one access rule (`DELETE`).
    pub const ADMIN_ACL_RULE: &str = "/api/v1/admin/acl/{id}";
    /// Admin: garbage collection of a depot.
    pub const ADMIN_GC: &str = "/api/v1/admin/depots/{d}/gc";
    /// Admin: consistency check of a depot.
    pub const ADMIN_FSCK: &str = "/api/v1/admin/depots/{d}/fsck";
    /// Locks module.
    pub const LOCKS: &str = "/api/v1/depots/{d}/locks";
    /// Shelves module: list and create.
    pub const SHELVES: &str = "/api/v1/depots/{d}/shelves";
    /// Shelves module: one shelf.
    pub const SHELF: &str = "/api/v1/depots/{d}/shelves/{id}";
    /// Shelves module: submit.
    pub const SHELF_SUBMIT: &str = "/api/v1/depots/{d}/shelves/{id}/submit";
}
