// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlRevision.h"

#include "LodeTypes.h"

/** One revision of a file, for the editor's history window and diffs. */
class FLodeRevision : public ISourceControlRevision
{
public:
	FLodeRevision(const FString& InFilename, const FLodeRevisionRecord& InRecord);

	virtual bool Get(FString& InOutFilename, EConcurrency::Type InConcurrency = EConcurrency::Synchronous) const override;
	virtual bool GetAnnotated(TArray<FAnnotationLine>& OutLines) const override;
	virtual bool GetAnnotated(FString& InOutFilename) const override;
	virtual const FString& GetFilename() const override;
	virtual int32 GetRevisionNumber() const override;
	virtual const FString& GetRevision() const override;
	virtual const FString& GetDescription() const override;
	virtual const FString& GetUserName() const override;
	virtual const FString& GetClientSpec() const override;
	virtual const FString& GetAction() const override;
	virtual TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> GetBranchSource() const override;
	virtual const FDateTime& GetDate() const override;
	virtual int32 GetCheckInIdentifier() const override;
	virtual int32 GetFileSize() const override;

	/** The depot path as it was in this revision. */
	const FString& GetDepotPath() const { return Record.DepotPath; }

private:
	/** Absolute filename of the file today (the editor's name for it). */
	FString Filename;
	FLodeRevisionRecord Record;
	FString RevisionText;
	FString UserText;
	FString ActionText;
	FString EmptyText;
	TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> BranchSource;
};

using FLodeRevisionRef = TSharedRef<FLodeRevision, ESPMode::ThreadSafe>;
