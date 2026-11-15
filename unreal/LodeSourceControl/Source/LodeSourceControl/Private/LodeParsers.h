// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"

#include "LodeTypes.h"

/**
 * Turns `lode --json` documents into plain records. No process, no editor state: these run on
 * worker threads and in tests. Every field read is named in LodeJsonFields.h.
 */
namespace LodeParsers
{
	ELodeFileStatus ParseStatusValue(const FString& Value);

	/**
	 * `lode status --unchanged [--remote]`. Records come back for every file lode listed, and for
	 * every requested file it did not list: those exist only as ignored files (on disk) or not at
	 * all. Requested folders produce no record of their own.
	 */
	void ParseStatus(const FJsonObject& Json, const FString& WorkspaceRoot, const TArray<FString>& RequestedFiles,
		TArray<FLodeFileRecord>& OutRecords, FString& OutBranch, int32& OutRev);

	/** `lode locks`. */
	TArray<FLodeLockRecord> ParseLocks(const FJsonObject& Json, const FString& WorkspaceRoot);

	/**
	 * `lode log <path> -v --follow`: the revisions that touched the file, newest first, following
	 * renames and copies back through `copy_from`.
	 */
	TArray<FLodeRevisionRecord> ParseFileLog(const FJsonObject& Json, const FString& DepotPath);

	/** `lode changelist list`. */
	TArray<FLodeChangelistRecord> ParseChangelists(const FJsonObject& Json, const FString& WorkspaceRoot);

	/** `lode shelves`, or one `lode shelf show` document (with its changes). */
	TArray<FLodeShelfRecord> ParseShelves(const FJsonObject& Json);
	FLodeShelfRecord ParseShelf(const FJsonObject& Json, const FString& WorkspaceRoot);

	/** `lode update` and `lode unshelve`: the files left in conflict. */
	TArray<FString> ParseConflicts(const FJsonObject& Json, const FString& WorkspaceRoot);

	/** A principal object `{ "name", "kind" }`; empty when absent or null. */
	FString PrincipalName(const TSharedPtr<FJsonObject>& Principal);

	FDateTime ParseTime(const FString& Rfc3339);
}
