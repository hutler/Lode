// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeState.h"

#if SOURCE_CONTROL_WITH_SLATE
#include "RevisionControlStyle/RevisionControlStyle.h"
#include "Textures/SlateIcon.h"
#endif

#define LOCTEXT_NAMESPACE "LodeSourceControl.State"

FLodeState::FLodeState(const FString& InFilename)
	: TimeStamp(0)
{
	Record.Filename = InFilename;
}

void FLodeState::SetRecord(const FLodeFileRecord& InRecord)
{
	FLodeFileRecord Next = InRecord;
	Next.KeepRemoteFrom(Record);
	Record = MoveTemp(Next);
	TimeStamp = FDateTime::Now();
}

void FLodeState::SetLockOwner(const FString& InOwner, bool bInMine)
{
	Record.bLockedHere = bInMine;
	Record.LockOwner = bInMine ? FString() : InOwner;
	TimeStamp = FDateTime::Now();
}

bool FLodeState::HasLocalChange() const
{
	switch (Record.Status)
	{
	case ELodeFileStatus::Added:
	case ELodeFileStatus::Modified:
	case ELodeFileStatus::Deleted:
	case ELodeFileStatus::Missing:
	case ELodeFileStatus::Conflicted:
		return true;
	default:
		return false;
	}
}

int32 FLodeState::GetHistorySize() const
{
	return History.Num();
}

TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FLodeState::GetHistoryItem(int32 HistoryIndex) const
{
	return History.IsValidIndex(HistoryIndex) ? TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe>(History[HistoryIndex]) : nullptr;
}

TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FLodeState::FindHistoryRevision(int32 RevisionNumber) const
{
	for (const FLodeRevisionRef& Revision : History)
	{
		if (Revision->GetRevisionNumber() == RevisionNumber)
		{
			return Revision;
		}
	}
	return nullptr;
}

TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FLodeState::FindHistoryRevision(const FString& InRevision) const
{
	const FString Digits = InRevision.StartsWith(TEXT("r")) ? InRevision.RightChop(1) : InRevision;
	return Digits.IsNumeric() ? FindHistoryRevision(FCString::Atoi(*Digits)) : nullptr;
}

TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FLodeState::GetCurrentRevision() const
{
	// The newest revision at or below the workspace's: the one the local file came from.
	for (const FLodeRevisionRef& Revision : History)
	{
		if (Record.WorkspaceRev == 0 || Revision->GetRevisionNumber() <= Record.WorkspaceRev)
		{
			return Revision;
		}
	}
	return nullptr;
}

ISourceControlState::FResolveInfo FLodeState::GetResolveInfo() const
{
	// Lode keeps no base for a binary conflict: the choice is this side or the other.
	return FResolveInfo();
}

#if SOURCE_CONTROL_WITH_SLATE
FSlateIcon FLodeState::GetIcon() const
{
	const FName Style = FRevisionControlStyleManager::GetStyleSetName();
	if (IsConflicted())
	{
		return FSlateIcon(Style, "RevisionControl.Conflicted");
	}
	if (IsLockedByOther())
	{
		return FSlateIcon(Style, "RevisionControl.CheckedOutByOtherUser", NAME_None, "RevisionControl.CheckedOutByOtherUserBadge");
	}
	if (!IsCurrent())
	{
		return FSlateIcon(Style, "RevisionControl.NotAtHeadRevision");
	}
	switch (Record.Status)
	{
	case ELodeFileStatus::Added:
		return Record.CopyFromPath.IsEmpty()
			? FSlateIcon(Style, "RevisionControl.OpenForAdd")
			: FSlateIcon(Style, "RevisionControl.Branched");
	case ELodeFileStatus::Deleted:
	case ELodeFileStatus::Missing:
		return FSlateIcon(Style, "RevisionControl.MarkedForDelete");
	case ELodeFileStatus::Modified:
		return FSlateIcon(Style, "RevisionControl.CheckedOut");
	case ELodeFileStatus::Untracked:
		return FSlateIcon(Style, "RevisionControl.NotInDepot");
	default:
		break;
	}
	if (Record.bLockedHere)
	{
		return FSlateIcon(Style, "RevisionControl.CheckedOut");
	}
	return FSlateIcon();
}
#endif

FText FLodeState::GetStateText() const
{
	if (IsLockedByOther())
	{
		return FText::Format(LOCTEXT("LockedByOther", "Locked by {0}"), FText::FromString(Record.LockOwner));
	}
	switch (Record.Status)
	{
	case ELodeFileStatus::Unknown: return LOCTEXT("Unknown", "Unknown");
	case ELodeFileStatus::Untracked: return LOCTEXT("Untracked", "Not in Lode");
	case ELodeFileStatus::Ignored: return LOCTEXT("Ignored", "Ignored");
	case ELodeFileStatus::Added: return Record.CopyFromPath.IsEmpty() ? LOCTEXT("Added", "Added") : (Record.bMoved ? LOCTEXT("MovedHere", "Moved") : LOCTEXT("Copied", "Copied"));
	case ELodeFileStatus::Modified: return Record.bLockedHere ? LOCTEXT("ModifiedLocked", "Modified, locked by me") : LOCTEXT("Modified", "Modified");
	case ELodeFileStatus::Deleted: return LOCTEXT("Deleted", "Marked for delete");
	case ELodeFileStatus::Missing: return LOCTEXT("Missing", "Missing from disk");
	case ELodeFileStatus::Conflicted: return LOCTEXT("Conflicted", "Conflicted");
	case ELodeFileStatus::Unchanged:
	default:
		break;
	}
	if (Record.bLockedHere)
	{
		return LOCTEXT("LockedByMe", "Locked by me");
	}
	if (!IsCurrent())
	{
		return LOCTEXT("NotAtHead", "Not at head");
	}
	if (Record.bLockRequired)
	{
		return LOCTEXT("LockRequired", "Needs a lock to edit");
	}
	return LOCTEXT("Unchanged", "Unchanged");
}

FText FLodeState::GetDisplayName() const
{
	return GetStateText();
}

FText FLodeState::GetDisplayTooltip() const
{
	TArray<FText> Lines;
	Lines.Add(GetStateText());
	if (IsLockedByOther())
	{
		Lines.Add(FText::Format(LOCTEXT("LockedByOtherTip", "{0} holds the lock; you cannot change this asset until they submit or release it."), FText::FromString(Record.LockOwner)));
	}
	if (Record.bNeedsLock)
	{
		Lines.Add(LOCTEXT("NeedsLockTip", "Changed without a lock: check it out (lock it) before you submit."));
	}
	else if (Record.bLockRequired && !Record.bLockedHere && Record.Status == ELodeFileStatus::Unchanged)
	{
		Lines.Add(LOCTEXT("LockRequiredTip", "Check out to take the lock before editing."));
	}
	if (Record.bNewerAtHead)
	{
		Lines.Add(Record.bDeletedAtHead
			? LOCTEXT("DeletedAtHeadTip", "Deleted on the server: Sync to get the latest.")
			: FText::Format(LOCTEXT("NewerTip", "A newer version is on the server (r{0}): Sync to get it."), FText::AsNumber(Record.HeadRev)));
	}
	if (!Record.Changelist.IsEmpty())
	{
		Lines.Add(FText::Format(LOCTEXT("ChangelistTip", "Changelist: {0}"), FText::FromString(Record.Changelist)));
	}
	return FText::Join(FText::FromString(TEXT("\n")), Lines);
}

const FString& FLodeState::GetFilename() const
{
	return Record.Filename;
}

const FDateTime& FLodeState::GetTimeStamp() const
{
	return TimeStamp;
}

bool FLodeState::CanCheckIn() const
{
	return HasLocalChange() && !IsConflicted() && !IsLockedByOther() && !Record.bNeedsLock;
}

bool FLodeState::CanCheckout() const
{
	// Only files that Lode keeps read-only need a lock before editing.
	return IsSourceControlled() && Record.bLockRequired && !Record.bLockedHere && !IsLockedByOther()
		&& Record.Status != ELodeFileStatus::Deleted && Record.Status != ELodeFileStatus::Added;
}

bool FLodeState::IsCheckedOut() const
{
	return Record.bLockedHere || Record.Status == ELodeFileStatus::Modified || Record.Status == ELodeFileStatus::Conflicted;
}

bool FLodeState::IsCheckedOutOther(FString* Who) const
{
	if (IsLockedByOther())
	{
		if (Who)
		{
			*Who = Record.LockOwner;
		}
		return true;
	}
	return false;
}

bool FLodeState::IsCurrent() const
{
	return !Record.bNewerAtHead;
}

bool FLodeState::IsSourceControlled() const
{
	return Record.Status != ELodeFileStatus::Unknown && Record.Status != ELodeFileStatus::Untracked && Record.Status != ELodeFileStatus::Ignored;
}

bool FLodeState::IsAdded() const
{
	return Record.Status == ELodeFileStatus::Added;
}

bool FLodeState::IsDeleted() const
{
	return Record.Status == ELodeFileStatus::Deleted || Record.Status == ELodeFileStatus::Missing;
}

bool FLodeState::IsIgnored() const
{
	return Record.Status == ELodeFileStatus::Ignored;
}

bool FLodeState::CanEdit() const
{
	if (IsLockedByOther())
	{
		return false;
	}
	if (!IsSourceControlled() || Record.Status == ELodeFileStatus::Added)
	{
		return true;
	}
	return !Record.bLockRequired || Record.bLockedHere;
}

bool FLodeState::IsUnknown() const
{
	return Record.Status == ELodeFileStatus::Unknown;
}

bool FLodeState::IsModified() const
{
	return HasLocalChange();
}

bool FLodeState::CanAdd() const
{
	// Files outside the workspace are Untracked too, but lode cannot add them.
	return Record.Status == ELodeFileStatus::Untracked && !Record.DepotPath.IsEmpty();
}

bool FLodeState::CanDelete() const
{
	return IsSourceControlled() && !IsDeleted() && !IsAdded() && !IsLockedByOther();
}

bool FLodeState::IsConflicted() const
{
	return Record.Status == ELodeFileStatus::Conflicted;
}

bool FLodeState::CanRevert() const
{
	return HasLocalChange() || Record.bLockedHere;
}

#undef LOCTEXT_NAMESPACE
