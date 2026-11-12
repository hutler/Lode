//! Helpers shared by the commands that work on local changes: opening a finished workspace,
//! turning command-line paths into depot paths, scanning, and getting base content back from
//! the blob cache or the server.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lode_client::{Client, ServerUrl};
use lode_core::{DepotPath, EntryKind, ObjectId};
use lode_workspace::db::keys;
use lode_workspace::scan::stat_for_row;
use lode_workspace::{
    BlobCache, ConflictRow, FileRow, FileState, ScanOptions, ScanReport, Workspace,
    DEFAULT_CACHE_CAP,
};
use serde_json::json;

use crate::cmd::checkout::CHECKOUT_REV;
use crate::context::Ctx;
use crate::error::{CliError, CliResult};
use crate::materialize::{place_file, place_symlink, prepare_target};

/// Largest symlink target accepted from the server.
const MAX_SYMLINK: usize = 64 * 1024;

/// The workspace around the working directory, refusing one whose checkout did not finish.
pub fn open(ctx: &Ctx) -> CliResult<Workspace> {
    let ws = ctx.workspace()?;
    if ws.db().config(CHECKOUT_REV)?.is_some() {
        return Err(CliError::failed(
            "checkout_incomplete",
            "the checkout of this workspace did not finish; rerun `lode checkout` first",
        ));
    }
    Ok(ws)
}

/// Command-line paths as depot paths. An empty result means the whole workspace (no paths
/// given, or one of them is the root).
pub fn depot_paths(ctx: &Ctx, ws: &Workspace, args: &[PathBuf]) -> CliResult<Vec<DepotPath>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        match lode_workspace::paths::to_depot(ws.root(), &ctx.cwd, a)? {
            Some(p) => out.push(p),
            None => return Ok(Vec::new()),
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Like [`depot_paths`], but every argument must name something below the root.
pub fn depot_path(ctx: &Ctx, ws: &Workspace, arg: &Path) -> CliResult<DepotPath> {
    lode_workspace::paths::to_depot(ws.root(), &ctx.cwd, arg)?.ok_or_else(|| {
        CliError::usage(format!(
            "{} is the workspace root; name a file or directory",
            arg.display()
        ))
    })
}

/// Whether `path` is `scope` or below it.
pub fn under(path: &DepotPath, scope: &DepotPath) -> bool {
    let (p, s) = (path.as_str(), scope.as_str());
    p == s || (p.starts_with(s) && p.as_bytes().get(s.len()) == Some(&b'/'))
}

/// Scans `paths` (all when empty), printing the scan's warnings.
pub fn scan(
    ctx: &Ctx,
    ws: &mut Workspace,
    paths: &[DepotPath],
    report_ignored: bool,
) -> CliResult<ScanReport> {
    let report = ws.scan(&ScanOptions {
        paths: paths.to_vec(),
        report_ignored,
        ..Default::default()
    })?;
    for w in &report.warnings {
        ctx.out.warn(w);
    }
    for bad in &report.invalid {
        ctx.out
            .warn(format!("skipping {}: {}", bad.local.display(), bad.reason));
    }
    Ok(report)
}

/// The revision the workspace was synced to.
pub fn synced_rev(ws: &Workspace) -> CliResult<u64> {
    ws.db().config_u64(keys::SYNCED_REV)?.ok_or_else(|| {
        CliError::failed(
            "checkout_incomplete",
            "this workspace has no synced revision",
        )
    })
}

/// The local file holding the incoming side of a binary conflict.
pub fn theirs_path(local: &Path, rev: u64) -> PathBuf {
    let mut name = local.as_os_str().to_owned();
    name.push(format!(".theirs-r{rev}"));
    PathBuf::from(name)
}

/// Where `.lode/conflicts/` keeps the local content `id` set aside by a text conflict.
pub fn mine_path(ws: &Workspace, id: &ObjectId) -> PathBuf {
    ws.layout().conflicts().join(id.to_hex())
}

/// Forgets a conflict: its row, its `.theirs-r<N>` file and the content it set aside.
pub fn clear_conflict(ws: &mut Workspace, c: &ConflictRow) -> CliResult<()> {
    remove_if_exists(&theirs_path(&ws.local_path(&c.path), c.theirs_rev))?;
    ws.db_mut().write(|tx| tx.remove_conflict(&c.path))?;
    if let Some(id) = c.mine_id {
        // Two conflicts can set aside the same content.
        if !ws.db().conflicts()?.iter().any(|o| o.mine_id == Some(id)) {
            remove_if_exists(&mine_path(ws, &id))?;
        }
    }
    Ok(())
}

/// Removes a file if it is there.
pub fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Base content: the workspace blob cache first, the server (signed in lazily) when it is not
/// there. Offline commands never touch the network unless they need a base.
pub struct Bases<'a> {
    ctx: &'a Ctx,
    server: ServerUrl,
    depot: String,
    cache: BlobCache,
    tmp: PathBuf,
    client: Option<Client>,
}

impl<'a> Bases<'a> {
    /// Bases for `ws`.
    pub fn new(ctx: &'a Ctx, ws: &Workspace) -> CliResult<Self> {
        let depot = ws
            .db()
            .config(keys::DEPOT)?
            .ok_or_else(|| CliError::failed("workspace", "workspace has no depot"))?;
        Ok(Self {
            ctx,
            server: ctx.server_url(Some(ws))?,
            depot,
            cache: ws.cache(DEFAULT_CACHE_CAP),
            tmp: ws.layout().tmp(),
            client: None,
        })
    }

    fn client(&mut self) -> CliResult<&Client> {
        if self.client.is_none() {
            self.client = Some(self.ctx.client(&self.server)?);
        }
        self.client
            .as_ref()
            .ok_or_else(|| CliError::failed("internal", "no client"))
    }

    /// The content of `id` in memory, cached for later diffs and merges.
    pub async fn read(&mut self, id: &ObjectId) -> CliResult<Vec<u8>> {
        if let Some(p) = self.cache.get(id)? {
            return Ok(fs::read(p)?);
        }
        let depot = self.depot.clone();
        let mut buf = Vec::new();
        self.client()?.get_blob_to(&depot, id, &mut buf).await?;
        if let Err(e) = self.cache.insert_reader(id, &buf[..]) {
            self.ctx.out.warn(format!("blob cache: {e}"));
        }
        Ok(buf)
    }

    /// Writes the content of `id` to a new temporary file in `.lode/tmp` and returns it.
    /// Large files are not put in the cache: they would only evict the useful entries.
    async fn fetch_tmp(&mut self, id: &ObjectId) -> CliResult<PathBuf> {
        fs::create_dir_all(&self.tmp)?;
        let tmp = self
            .tmp
            .join(format!("{}-{}", id.to_hex(), std::process::id()));
        if let Some(p) = self.cache.get(id)? {
            fs::copy(p, &tmp)?;
            return Ok(tmp);
        }
        let depot = self.depot.clone();
        self.client()?.download_blob(&depot, id, &tmp).await?;
        Ok(tmp)
    }

    /// Puts `row`'s base content and mode back on disk and marks the row unchanged.
    pub async fn restore(&mut self, ws: &mut Workspace, row: &FileRow) -> CliResult<()> {
        let id = row.base_id.ok_or_else(|| {
            CliError::failed("internal", format!("{} has no base to restore", row.path))
        })?;
        let root = ws.root().to_path_buf();
        let target = prepare_target(&root, &row.path)?;
        if row.kind == EntryKind::Symlink {
            let bytes = self.read(&id).await?;
            if bytes.len() > MAX_SYMLINK {
                return Err(CliError::failed(
                    "protocol",
                    format!("symlink {} is too long", row.path),
                ));
            }
            let text = String::from_utf8(bytes).map_err(|_| {
                CliError::failed("protocol", format!("symlink {} is not UTF-8", row.path))
            })?;
            place_symlink(&text, &target)?;
        } else {
            let tmp = self.fetch_tmp(&id).await?;
            place_file(&tmp, &target, row.executable)?;
        }
        let (size, mtime) = stat_for_row(&target, SystemTime::now())?;
        let mut fresh = row.clone();
        fresh.state = FileState::Normal;
        fresh.size = size;
        fresh.mtime_ns = mtime;
        ws.db_mut().write(|tx| tx.put_file(&fresh))?;
        Ok(())
    }
}

/// A `not_found` error naming `paths`.
pub fn not_found(what: &str, paths: &[String]) -> CliError {
    CliError::failed("not_found", format!("{what}: {}", paths.join(", ")))
        .with_details(json!({ "paths": paths }))
}
