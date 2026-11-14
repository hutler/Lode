// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlChangelist.h"
#include "ISourceControlChangelistState.h"
#include "ISourceControlState.h"

/** A Lode changelist, named. Lode's "default" changelist holds the files that are in no other. */
class FLodeChangelist : public ISourceControlChangelist
{
public:
	FLodeChangelist() = default;
	explicit FLodeChangelist(const FString& InName) : Name(InName) {}

	static const FString DefaultName;
	static FLodeChangelist Default() { return FLodeChangelist(DefaultName); }

	const FString& GetName() const { return Name; }
	bool IsInitialized() const { return !Name.IsEmpty(); }

	virtual bool CanDelete() const override { return !IsDefault(); }
	virtual bool IsDefault() const override { return Name.IsEmpty() || Name == DefaultName; }
	virtual FString GetIdentifier() const override { return Name; }

	bool operator==(const FLodeChangelist& Other) const { return Name.Equals(Other.Name, ESearchCase::CaseSensitive); }
	bool operator!=(const FLodeChangelist& Other) const { return !(*this == Other); }
	friend uint32 GetTypeHash(const FLodeChangelist& Changelist) { return GetTypeHash(Changelist.Name); }

private:
	FString Name;
};

using FLodeChangelistRef = TSharedRef<FLodeChangelist, ESPMode::ThreadSafe>;

/** What the View Changes window shows for one changelist. */
class FLodeChangelistState : public ISourceControlChangelistState
{
public:
	explicit FLodeChangelistState(const FLodeChangelist& InChangelist) : Changelist(InChangelist), TimeStamp(0) {}

	virtual FName GetIconName() const override;
	virtual FName GetSmallIconName() const override;
	virtual FText GetDisplayText() const override;
	virtual FText GetDescriptionText() const override;
	virtual FText GetDisplayTooltip() const override;
	virtual const FDateTime& GetTimeStamp() const override;
	virtual const TArray<FSourceControlStateRef> GetFilesStates() const override;
	virtual int32 GetFilesStatesNum() const override;
	virtual const TArray<FSourceControlStateRef> GetShelvedFilesStates() const override;
	virtual int32 GetShelvedFilesStatesNum() const override;
	virtual FSourceControlChangelistRef GetChangelist() const override;

	FLodeChangelist Changelist;
	FString Description;
	TArray<FSourceControlStateRef> Files;
	/** The shelf that holds this changelist's shelved files, 0 when none. */
	int32 ShelfId = 0;
	int32 ShelfVersion = 0;
	TArray<FSourceControlStateRef> ShelvedFiles;
	FDateTime TimeStamp;
};

using FLodeChangelistStateRef = TSharedRef<FLodeChangelistState, ESPMode::ThreadSafe>;
