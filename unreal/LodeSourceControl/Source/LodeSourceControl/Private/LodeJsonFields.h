// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"

/**
 * Every JSON field the plugin reads from `lode --json`, in one place.
 *
 * Source of truth: lode/docs/CLI-JSON.md (Lode 0.2.0). The plugin must not read a field that is
 * not listed here; when Lode's document changes, this file is the one to correct. Fields marked
 * "observed" are printed by lode 0.2.0 but not named in CLI-JSON.md.
 */
namespace LodeJson
{
	// Envelope (CLI-JSON "Envelope")
	inline constexpr const TCHAR* Ok = TEXT("ok");
	inline constexpr const TCHAR* Error = TEXT("error");
	inline constexpr const TCHAR* Code = TEXT("code");
	inline constexpr const TCHAR* Message = TEXT("message");
	inline constexpr const TCHAR* Details = TEXT("details");
	inline constexpr const TCHAR* Paths = TEXT("paths");
	inline constexpr const TCHAR* Locks = TEXT("locks"); // also details.locks of a `locked` error (observed)

	// lode --version
	inline constexpr const TCHAR* Version = TEXT("version");
	inline constexpr const TCHAR* Api = TEXT("api");

	// lode whoami / login
	inline constexpr const TCHAR* Server = TEXT("server");
	inline constexpr const TCHAR* Name = TEXT("name");
	inline constexpr const TCHAR* Kind = TEXT("kind");
	inline constexpr const TCHAR* Scopes = TEXT("scopes");

	// lode status [--unchanged] [--remote]
	inline constexpr const TCHAR* Branch = TEXT("branch");
	inline constexpr const TCHAR* Rev = TEXT("rev");
	inline constexpr const TCHAR* Files = TEXT("files");
	inline constexpr const TCHAR* Invalid = TEXT("invalid");
	inline constexpr const TCHAR* Merges = TEXT("merges");
	inline constexpr const TCHAR* Path = TEXT("path");
	inline constexpr const TCHAR* Status = TEXT("status");
	inline constexpr const TCHAR* CopyFrom = TEXT("copy_from");
	inline constexpr const TCHAR* Moved = TEXT("moved");
	inline constexpr const TCHAR* Changelist = TEXT("changelist");
	inline constexpr const TCHAR* Locked = TEXT("locked");
	inline constexpr const TCHAR* NeedsLock = TEXT("needs_lock");
	inline constexpr const TCHAR* LockRequired = TEXT("lock_required");
	inline constexpr const TCHAR* Local = TEXT("local");
	inline constexpr const TCHAR* Reason = TEXT("reason");
	inline constexpr const TCHAR* Remote = TEXT("remote");
	inline constexpr const TCHAR* HeadRev = TEXT("head_rev");
	inline constexpr const TCHAR* Newer = TEXT("newer");
	inline constexpr const TCHAR* DeletedAtHead = TEXT("deleted_at_head");
	inline constexpr const TCHAR* LockedBy = TEXT("locked_by");
	inline constexpr const TCHAR* LockWorkspace = TEXT("lock_workspace");
	inline constexpr const TCHAR* Mine = TEXT("mine");

	// status values
	inline constexpr const TCHAR* StatusModified = TEXT("modified");
	inline constexpr const TCHAR* StatusAdded = TEXT("added");
	inline constexpr const TCHAR* StatusDeleted = TEXT("deleted");
	inline constexpr const TCHAR* StatusMissing = TEXT("missing");
	inline constexpr const TCHAR* StatusConflicted = TEXT("conflicted");
	inline constexpr const TCHAR* StatusUntracked = TEXT("untracked");
	inline constexpr const TCHAR* StatusUnchanged = TEXT("unchanged");

	// lode locks
	inline constexpr const TCHAR* Owner = TEXT("owner");
	inline constexpr const TCHAR* Workspace = TEXT("workspace");
	inline constexpr const TCHAR* Comment = TEXT("comment");
	inline constexpr const TCHAR* CreatedAt = TEXT("created_at");

	// lode log (revision objects)
	inline constexpr const TCHAR* Depot = TEXT("depot");
	inline constexpr const TCHAR* Revisions = TEXT("revisions");
	inline constexpr const TCHAR* More = TEXT("more");
	inline constexpr const TCHAR* Author = TEXT("author");
	inline constexpr const TCHAR* Submitter = TEXT("submitter");
	inline constexpr const TCHAR* Changes = TEXT("changes");
	inline constexpr const TCHAR* ChangesTruncated = TEXT("changes_truncated");
	inline constexpr const TCHAR* Action = TEXT("action");
	inline constexpr const TCHAR* Size = TEXT("size");

	// lode cat -o
	inline constexpr const TCHAR* Output = TEXT("output");

	// lode commit
	inline constexpr const TCHAR* Uploaded = TEXT("uploaded");

	// lode update / unshelve
	inline constexpr const TCHAR* Outcome = TEXT("outcome");
	inline constexpr const TCHAR* Counts = TEXT("counts");
	inline constexpr const TCHAR* OutcomeConflicted = TEXT("conflicted");

	// lode changelist list
	inline constexpr const TCHAR* Changelists = TEXT("changelists");
	inline constexpr const TCHAR* Description = TEXT("description");

	// lode shelve / shelves / shelf show
	inline constexpr const TCHAR* Shelf = TEXT("shelf");
	inline constexpr const TCHAR* Shelves = TEXT("shelves");
	inline constexpr const TCHAR* Id = TEXT("id");
	inline constexpr const TCHAR* State = TEXT("state");
	inline constexpr const TCHAR* BaseRev = TEXT("base_rev");
	inline constexpr const TCHAR* UpdatedAt = TEXT("updated_at");
}

/** Error codes the plugin treats specially (CLI-JSON "Exit codes"). */
namespace LodeErrorCode
{
	inline constexpr const TCHAR* NotAWorkspace = TEXT("not_a_workspace");
	inline constexpr const TCHAR* OutOfDate = TEXT("out_of_date");
	inline constexpr const TCHAR* Locked = TEXT("locked");
	inline constexpr const TCHAR* LockRequired = TEXT("lock_required");
	inline constexpr const TCHAR* Conflicted = TEXT("conflicted");
	inline constexpr const TCHAR* NothingToCommit = TEXT("nothing_to_commit");
	inline constexpr const TCHAR* ModuleDisabled = TEXT("module_disabled");
	inline constexpr const TCHAR* Unauthenticated = TEXT("unauthenticated");
	inline constexpr const TCHAR* PermissionDenied = TEXT("permission_denied");
	inline constexpr const TCHAR* Network = TEXT("network");
	inline constexpr const TCHAR* NotFound = TEXT("not_found");
	inline constexpr const TCHAR* WorkspaceInUse = TEXT("workspace_in_use");
}
