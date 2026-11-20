// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlState.h"

#include "LodeRevision.h"
#include "LodeTypes.h"

/**
 * A file's revision control state as the editor sees it.
 *
 * Lode's rule for locks: a file that matches [locks].required stays read-only until it is
 * locked, and "check out" means taking that lock. Every other file is simply edited; its local
 * change counts as checked out so that it shows in the submit window.
 */
class FLodeState : public ISourceControlState
{
public:
	explicit FLodeState(const FString& InFilename);

	const FLodeFileRecord& GetRecord() const { return Record; }
	void SetRecord(const FLodeFileRecord& InRecord);
	void SetHistory(TArray<FLodeRevisionRef>&& InHistory) { History = MoveTemp(InHistory); }
	void SetLockOwner(const FString& InOwner, bool bInMine);

	/** Text for the content browser tooltip, also used in tests. */
	FText GetStateText() const;

	virtual int32 GetHistorySize() const override;
	virtual TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> GetHistoryItem(int32 HistoryIndex) const override;
	virtual TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FindHistoryRevision(int32 RevisionNumber) const override;
	virtual TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FindHistoryRevision(const FString& InRevision) const override;
	virtual TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> GetCurrentRevision() const override;
	virtual FResolveInfo GetResolveInfo() const override;
#if SOURCE_CONTROL_WITH_SLATE
	virtual FSlateIcon GetIcon() const override;
#endif
	virtual FText GetDisplayName() const override;
	virtual FText GetDisplayTooltip() const override;
	virtual const FString& GetFilename() const override;
	virtual const FDateTime& GetTimeStamp() const override;
	virtual bool CanCheckIn() const override;
	virtual bool CanCheckout() const override;
	virtual bool IsCheckedOut() const override;
	virtual bool IsCheckedOutOther(FString* Who = nullptr) const override;
	virtual bool IsCheckedOutInOtherBranch(const FString& CurrentBranch = FString()) const override { return false; }
	virtual bool IsModifiedInOtherBranch(const FString& CurrentBranch = FString()) const override { return false; }
	virtual bool IsCheckedOutOrModifiedInOtherBranch(const FString& CurrentBranch = FString()) const override { return false; }
	virtual TArray<FString> GetCheckedOutBranches() const override { return TArray<FString>(); }
	virtual FString GetOtherUserBranchCheckedOuts() const override { return FString(); }
	virtual bool GetOtherBranchHeadModification(FString& HeadBranchOut, FString& ActionOut, int32& HeadChangeListOut) const override { return false; }
	virtual bool IsCurrent() const override;
	virtual bool IsSourceControlled() const override;
	virtual bool IsAdded() const override;
	virtual bool IsDeleted() const override;
	virtual bool IsIgnored() const override;
	virtual bool CanEdit() const override;
	virtual bool IsUnknown() const override;
	virtual bool IsModified() const override;
	virtual bool CanAdd() const override;
	virtual bool CanDelete() const override;
	virtual bool IsConflicted() const override;
	virtual bool CanRevert() const override;

	/** A pending local change (added, modified, deleted, missing, conflicted). */
	bool HasLocalChange() const;
	/** Someone else holds the lock. */
	bool IsLockedByOther() const { return !Record.LockOwner.IsEmpty() && !Record.bLockedHere; }

private:
	FLodeFileRecord Record;
	TArray<FLodeRevisionRef> History;
	FDateTime TimeStamp;
};

using FLodeStateRef = TSharedRef<FLodeState, ESPMode::ThreadSafe>;
