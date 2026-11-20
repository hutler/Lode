// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeParsers.h"

#include "Misc/Paths.h"

#include "LodeJsonFields.h"
#include "LodePaths.h"

namespace
{
	FString StringOr(const FJsonObject& Object, const TCHAR* Field, const FString& Default = FString())
	{
		FString Value;
		return Object.TryGetStringField(Field, Value) ? Value : Default;
	}

	bool BoolOr(const FJsonObject& Object, const TCHAR* Field, bool bDefault = false)
	{
		bool bValue = bDefault;
		return Object.TryGetBoolField(Field, bValue) ? bValue : bDefault;
	}

	int32 IntOr(const FJsonObject& Object, const TCHAR* Field, int32 Default = 0)
	{
		int32 Value = Default;
		return Object.TryGetNumberField(Field, Value) ? Value : Default;
	}

	int64 Int64Or(const FJsonObject& Object, const TCHAR* Field, int64 Default = 0)
	{
		int64 Value = Default;
		return Object.TryGetNumberField(Field, Value) ? Value : Default;
	}

	TSharedPtr<FJsonObject> ObjectOrNull(const FJsonObject& Object, const TCHAR* Field)
	{
		const TSharedPtr<FJsonObject>* Value = nullptr;
		if (Object.TryGetObjectField(Field, Value) && Value)
		{
			return *Value;
		}
		return nullptr;
	}

	const TArray<TSharedPtr<FJsonValue>>* ArrayOrNull(const FJsonObject& Object, const TCHAR* Field)
	{
		const TArray<TSharedPtr<FJsonValue>>* Value = nullptr;
		return Object.TryGetArrayField(Field, Value) ? Value : nullptr;
	}

	template <typename FunctionType>
	void ForEachObject(const FJsonObject& Object, const TCHAR* Field, FunctionType&& Function)
	{
		if (const TArray<TSharedPtr<FJsonValue>>* Items = ArrayOrNull(Object, Field))
		{
			for (const TSharedPtr<FJsonValue>& Item : *Items)
			{
				const TSharedPtr<FJsonObject>* ItemObject = nullptr;
				if (Item.IsValid() && Item->TryGetObject(ItemObject) && ItemObject && ItemObject->IsValid())
				{
					Function(**ItemObject);
				}
			}
		}
	}
}

ELodeFileStatus LodeParsers::ParseStatusValue(const FString& Value)
{
	if (Value == LodeJson::StatusUnchanged) return ELodeFileStatus::Unchanged;
	if (Value == LodeJson::StatusModified) return ELodeFileStatus::Modified;
	if (Value == LodeJson::StatusAdded) return ELodeFileStatus::Added;
	if (Value == LodeJson::StatusDeleted) return ELodeFileStatus::Deleted;
	if (Value == LodeJson::StatusMissing) return ELodeFileStatus::Missing;
	if (Value == LodeJson::StatusConflicted) return ELodeFileStatus::Conflicted;
	if (Value == LodeJson::StatusUntracked) return ELodeFileStatus::Untracked;
	return ELodeFileStatus::Unknown;
}

FString LodeParsers::PrincipalName(const TSharedPtr<FJsonObject>& Principal)
{
	return Principal.IsValid() ? StringOr(*Principal, LodeJson::Name) : FString();
}

FDateTime LodeParsers::ParseTime(const FString& Rfc3339)
{
	FDateTime Out;
	if (!FDateTime::ParseIso8601(*Rfc3339, Out))
	{
		Out = FDateTime(0);
	}
	return Out;
}

void LodeParsers::ParseStatus(const FJsonObject& Json, const FString& WorkspaceRoot, const TArray<FString>& RequestedFiles,
	TArray<FLodeFileRecord>& OutRecords, FString& OutBranch, int32& OutRev)
{
	OutBranch = StringOr(Json, LodeJson::Branch);
	OutRev = IntOr(Json, LodeJson::Rev);

	TMap<FString, int32> IndexByDepotPath;
	ForEachObject(Json, LodeJson::Files, [&](const FJsonObject& File)
	{
		FLodeFileRecord Record;
		Record.DepotPath = StringOr(File, LodeJson::Path);
		if (Record.DepotPath.IsEmpty())
		{
			return;
		}
		Record.Filename = LodePaths::ToFilename(WorkspaceRoot, Record.DepotPath);
		Record.Status = ParseStatusValue(StringOr(File, LodeJson::Status));
		Record.Changelist = StringOr(File, LodeJson::Changelist);
		if (Record.Changelist == TEXT("default"))
		{
			Record.Changelist.Reset();
		}
		if (const TSharedPtr<FJsonObject> CopyFrom = ObjectOrNull(File, LodeJson::CopyFrom))
		{
			Record.CopyFromPath = StringOr(*CopyFrom, LodeJson::Path);
			Record.CopyFromRev = IntOr(*CopyFrom, LodeJson::Rev);
		}
		Record.bMoved = BoolOr(File, LodeJson::Moved);
		Record.bLockedHere = BoolOr(File, LodeJson::Locked);
		Record.bLockRequired = BoolOr(File, LodeJson::LockRequired);
		Record.bNeedsLock = BoolOr(File, LodeJson::NeedsLock);
		Record.WorkspaceRev = OutRev;
		// Copy the key first: argument order is unspecified, and the move may run before it is read.
		const FString Key = Record.DepotPath;
		IndexByDepotPath.Add(Key, OutRecords.Add(MoveTemp(Record)));
	});

	if (const TSharedPtr<FJsonObject> Remote = ObjectOrNull(Json, LodeJson::Remote))
	{
		const int32 HeadRev = IntOr(*Remote, LodeJson::HeadRev);
		// Files lode listed but the server said nothing about: at head, and nobody else holds a lock.
		for (FLodeFileRecord& Record : OutRecords)
		{
			Record.bRemoteKnown = true;
			Record.HeadRev = HeadRev;
		}
		ForEachObject(*Remote, LodeJson::Files, [&](const FJsonObject& File)
		{
			const FString DepotPath = StringOr(File, LodeJson::Path);
			int32* Index = IndexByDepotPath.Find(DepotPath);
			if (!Index)
			{
				FLodeFileRecord Record;
				Record.DepotPath = DepotPath;
				Record.Filename = LodePaths::ToFilename(WorkspaceRoot, DepotPath);
				// Not in the workspace, but on the server: new at head, or excluded by the view.
				Record.Status = ELodeFileStatus::Untracked;
				Record.bRemoteKnown = true;
				Record.HeadRev = HeadRev;
				Record.WorkspaceRev = OutRev;
				Index = &IndexByDepotPath.Add(DepotPath, OutRecords.Add(MoveTemp(Record)));
			}
			FLodeFileRecord& Record = OutRecords[*Index];
			Record.bNewerAtHead = BoolOr(File, LodeJson::Newer);
			Record.bDeletedAtHead = BoolOr(File, LodeJson::DeletedAtHead);
			const bool bMine = BoolOr(File, LodeJson::Mine);
			const FString Holder = PrincipalName(ObjectOrNull(File, LodeJson::LockedBy));
			Record.LockOwner = bMine ? FString() : Holder;
			if (bMine)
			{
				Record.bLockedHere = true;
			}
		});
	}

	// Requested files that lode did not list. With --unchanged lode lists every tracked file and
	// every untracked one it sees, so a file on disk that is missing from the list is ignored.
	for (const FString& Requested : RequestedFiles)
	{
		const FString Filename = LodePaths::Normalize(Requested);
		const FString DepotPath = LodePaths::ToDepotPath(WorkspaceRoot, Filename);
		if (DepotPath.IsEmpty() || IndexByDepotPath.Contains(DepotPath) || FPaths::DirectoryExists(Filename))
		{
			continue;
		}
		// A folder of listed files, even when it is not on disk (not synced yet, say).
		const FString FolderPrefix = DepotPath + TEXT("/");
		bool bIsFolder = false;
		for (const TPair<FString, int32>& Item : IndexByDepotPath)
		{
			if (Item.Key.StartsWith(FolderPrefix, ESearchCase::CaseSensitive))
			{
				bIsFolder = true;
				break;
			}
		}
		if (bIsFolder)
		{
			continue;
		}
		FLodeFileRecord Record;
		Record.Filename = Filename;
		Record.DepotPath = DepotPath;
		Record.Status = FPaths::FileExists(Filename) ? ELodeFileStatus::Ignored : ELodeFileStatus::Untracked;
		Record.WorkspaceRev = OutRev;
		IndexByDepotPath.Add(DepotPath, OutRecords.Add(MoveTemp(Record)));
	}
}

TArray<FLodeLockRecord> LodeParsers::ParseLocks(const FJsonObject& Json, const FString& WorkspaceRoot)
{
	TArray<FLodeLockRecord> Locks;
	ForEachObject(Json, LodeJson::Locks, [&](const FJsonObject& Lock)
	{
		FLodeLockRecord Record;
		Record.DepotPath = StringOr(Lock, LodeJson::Path);
		Record.Filename = LodePaths::ToFilename(WorkspaceRoot, Record.DepotPath);
		Record.Owner = PrincipalName(ObjectOrNull(Lock, LodeJson::Owner));
		Record.Comment = StringOr(Lock, LodeJson::Comment);
		Record.CreatedAt = ParseTime(StringOr(Lock, LodeJson::CreatedAt));
		Record.bMine = BoolOr(Lock, LodeJson::Mine);
		Locks.Add(MoveTemp(Record));
	});
	return Locks;
}

TArray<FLodeRevisionRecord> LodeParsers::ParseFileLog(const FJsonObject& Json, const FString& DepotPath)
{
	TArray<FLodeRevisionRecord> Revisions;
	FString Current = DepotPath;
	int32 CopiedAt = 0; // after following a copy, only revisions at or before this one count
	ForEachObject(Json, LodeJson::Revisions, [&](const FJsonObject& Revision)
	{
		const int32 Rev = IntOr(Revision, LodeJson::Rev);
		if (CopiedAt > 0 && Rev > CopiedAt)
		{
			return;
		}
		const FJsonObject* Change = nullptr;
		ForEachObject(Revision, LodeJson::Changes, [&](const FJsonObject& Candidate)
		{
			if (!Change && StringOr(Candidate, LodeJson::Path) == Current)
			{
				Change = &Candidate;
			}
		});
		// A full change list without this file: the revision is about other files.
		if (!Change && !BoolOr(Revision, LodeJson::ChangesTruncated))
		{
			return;
		}

		FLodeRevisionRecord Record;
		Record.Rev = Rev;
		Record.Branch = StringOr(Revision, LodeJson::Branch);
		Record.Author = PrincipalName(ObjectOrNull(Revision, LodeJson::Author));
		Record.Submitter = PrincipalName(ObjectOrNull(Revision, LodeJson::Submitter));
		Record.Date = ParseTime(StringOr(Revision, LodeJson::CreatedAt));
		Record.Message = StringOr(Revision, LodeJson::Message);
		Record.DepotPath = Current;
		if (Change)
		{
			Record.Action = StringOr(*Change, LodeJson::Action);
			Record.Size = Int64Or(*Change, LodeJson::Size);
			if (const TSharedPtr<FJsonObject> CopyFrom = ObjectOrNull(*Change, LodeJson::CopyFrom))
			{
				Record.CopyFromPath = StringOr(*CopyFrom, LodeJson::Path);
				Record.CopyFromRev = IntOr(*CopyFrom, LodeJson::Rev);
			}
		}
		else
		{
			// `--follow` listed this revision for the file but -v left the change out (a very
			// large revision is truncated): keep the revision, without details.
			Record.Action = TEXT("modify");
		}
		if (!Record.CopyFromPath.IsEmpty() && Record.CopyFromPath != Current)
		{
			Record.Action = Record.Action == TEXT("add") ? TEXT("branch") : Record.Action;
			Current = Record.CopyFromPath;
			CopiedAt = Record.CopyFromRev;
		}
		Revisions.Add(MoveTemp(Record));
	});
	return Revisions;
}

TArray<FLodeChangelistRecord> LodeParsers::ParseChangelists(const FJsonObject& Json, const FString& WorkspaceRoot)
{
	TArray<FLodeChangelistRecord> Changelists;
	ForEachObject(Json, LodeJson::Changelists, [&](const FJsonObject& Changelist)
	{
		FLodeChangelistRecord Record;
		Record.Name = StringOr(Changelist, LodeJson::Name);
		Record.Description = StringOr(Changelist, LodeJson::Description);
		TArray<FString> DepotPaths;
		Changelist.TryGetStringArrayField(LodeJson::Files, DepotPaths);
		for (const FString& DepotPath : DepotPaths)
		{
			Record.Filenames.Add(LodePaths::ToFilename(WorkspaceRoot, DepotPath));
		}
		Changelists.Add(MoveTemp(Record));
	});
	return Changelists;
}

namespace
{
	FLodeShelfRecord ParseShelfFields(const FJsonObject& Shelf)
	{
		FLodeShelfRecord Record;
		Record.Id = IntOr(Shelf, LodeJson::Id);
		Record.Version = IntOr(Shelf, LodeJson::Version);
		Record.Branch = StringOr(Shelf, LodeJson::Branch);
		Record.Owner = LodeParsers::PrincipalName(ObjectOrNull(Shelf, LodeJson::Owner));
		Record.Description = StringOr(Shelf, LodeJson::Description);
		Record.State = StringOr(Shelf, LodeJson::State);
		Record.BaseRev = IntOr(Shelf, LodeJson::BaseRev);
		Record.CreatedAt = LodeParsers::ParseTime(StringOr(Shelf, LodeJson::CreatedAt));
		Record.UpdatedAt = LodeParsers::ParseTime(StringOr(Shelf, LodeJson::UpdatedAt));
		return Record;
	}
}

TArray<FLodeShelfRecord> LodeParsers::ParseShelves(const FJsonObject& Json)
{
	TArray<FLodeShelfRecord> Shelves;
	ForEachObject(Json, LodeJson::Shelves, [&](const FJsonObject& Shelf)
	{
		Shelves.Add(ParseShelfFields(Shelf));
	});
	return Shelves;
}

FLodeShelfRecord LodeParsers::ParseShelf(const FJsonObject& Json, const FString& WorkspaceRoot)
{
	FLodeShelfRecord Record = ParseShelfFields(Json);
	ForEachObject(Json, LodeJson::Changes, [&](const FJsonObject& Change)
	{
		Record.Filenames.Add(LodePaths::ToFilename(WorkspaceRoot, StringOr(Change, LodeJson::Path)));
		Record.Actions.Add(StringOr(Change, LodeJson::Action));
	});
	return Record;
}

TArray<FString> LodeParsers::ParseConflicts(const FJsonObject& Json, const FString& WorkspaceRoot)
{
	TArray<FString> Conflicts;
	ForEachObject(Json, LodeJson::Files, [&](const FJsonObject& File)
	{
		if (StringOr(File, LodeJson::Outcome) == LodeJson::OutcomeConflicted)
		{
			Conflicts.Add(LodePaths::ToFilename(WorkspaceRoot, StringOr(File, LodeJson::Path)));
		}
	});
	return Conflicts;
}
