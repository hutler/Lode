// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"

/** A file's local state as `lode status` reports it, plus the cases status leaves out. */
enum class ELodeFileStatus : uint8
{
	Unknown,     // not asked about yet
	Untracked,   // not in the depot (lode: untracked), or outside the workspace
	Ignored,     // matches .lodeignore
	Unchanged,
	Added,
	Modified,
	Deleted,     // marked for delete
	Missing,     // tracked, gone from disk without `lode rm`
	Conflicted,
};

/** What the plugin knows about one file. Plain data: built on worker threads, copied into states on the game thread. */
struct FLodeFileRecord
{
	/** Absolute, normalized (forward slashes). The state cache key. */
	FString Filename;
	/** Depot path, relative to the workspace root. Empty for files outside the workspace. */
	FString DepotPath;

	ELodeFileStatus Status = ELodeFileStatus::Unknown;
	/** Changelist name; empty means "default". */
	FString Changelist;
	/** Depot path this file was copied or moved from, with its revision. */
	FString CopyFromPath;
	int32 CopyFromRev = 0;
	bool bMoved = false;

	/** This workspace holds the file's lock. */
	bool bLockedHere = false;
	/** Matches [locks].required in .lodeconfig (read-only until locked). */
	bool bLockRequired = false;
	/** A changed file that matches [locks].required and is not locked here: a submit would be refused. */
	bool bNeedsLock = false;

	/** Server information below is valid. */
	bool bRemoteKnown = false;
	bool bNewerAtHead = false;
	bool bDeletedAtHead = false;
	/** Who holds the lock when someone else does; empty otherwise. */
	FString LockOwner;
	/** The workspace revision the file is synced to, and the branch head, when known. */
	int32 WorkspaceRev = 0;
	int32 HeadRev = 0;

	/** Takes server information from an older record when this one has none. */
	void KeepRemoteFrom(const FLodeFileRecord& Older)
	{
		if (!bRemoteKnown && Older.bRemoteKnown)
		{
			bRemoteKnown = true;
			bNewerAtHead = Older.bNewerAtHead;
			bDeletedAtHead = Older.bDeletedAtHead;
			LockOwner = Older.LockOwner;
			HeadRev = Older.HeadRev;
		}
	}
};

/** One lock from `lode locks`. */
struct FLodeLockRecord
{
	FString Filename;
	FString DepotPath;
	FString Owner;
	FString Comment;
	FDateTime CreatedAt;
	bool bMine = false;
};

/** One revision of a file, from `lode log -v --follow`. */
struct FLodeRevisionRecord
{
	int32 Rev = 0;
	FString Branch;
	FString Author;
	FString Submitter;
	FDateTime Date;
	FString Message;
	/** add, modify, delete; "copy" and "move" are added by the plugin when the change has copy_from. */
	FString Action;
	int64 Size = 0;
	/** The depot path the file had in this revision (differs from today's after a rename). */
	FString DepotPath;
	FString CopyFromPath;
	int32 CopyFromRev = 0;
};

/** One changelist from `lode changelist list`. */
struct FLodeChangelistRecord
{
	FString Name;
	FString Description;
	TArray<FString> Filenames;
};

/** One shelf from `lode shelves` or `lode shelf show`. */
struct FLodeShelfRecord
{
	int32 Id = 0;
	int32 Version = 0;
	FString Branch;
	FString Owner;
	FString Description;
	FString State;
	int32 BaseRev = 0;
	FDateTime CreatedAt;
	FDateTime UpdatedAt;
	/** Absolute filenames of the shelf's changes (only from `shelf show`). */
	TArray<FString> Filenames;
	TArray<FString> Actions;
};

/** The workspace the editor's project lives in. */
struct FLodeWorkspaceInfo
{
	FString Root;
	FString Server;
	FString Depot;
	FString Branch;
	int32 Rev = 0;
	FString UserName;
	FString UserKind;
	TArray<FString> Scopes;
	FString ClientVersion;
	FString ClientApi;
};
