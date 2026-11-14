// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeRevision.h"

#include "HAL/FileManager.h"
#include "Misc/Paths.h"

#include "LodeLog.h"
#include "LodeSourceControlModule.h"

FLodeRevision::FLodeRevision(const FString& InFilename, const FLodeRevisionRecord& InRecord)
	: Filename(InFilename)
	, Record(InRecord)
{
	RevisionText = FString::Printf(TEXT("r%d"), Record.Rev);

	// History shows who wrote the change; when someone else submitted it (an agent's shelf
	// submitted by a person, say), both names appear.
	UserText = Record.Author;
	if (!Record.Submitter.IsEmpty() && Record.Submitter != Record.Author)
	{
		UserText = FString::Printf(TEXT("%s (submitted by %s)"), *Record.Author, *Record.Submitter);
	}

	if (Record.Action == TEXT("modify")) ActionText = TEXT("edit");
	else if (Record.Action.IsEmpty()) ActionText = TEXT("edit");
	else ActionText = Record.Action; // add, delete, branch

	if (!Record.CopyFromPath.IsEmpty())
	{
		FLodeRevisionRecord Source;
		Source.Rev = Record.CopyFromRev;
		Source.DepotPath = Record.CopyFromPath;
		Source.Action = TEXT("edit");
		BranchSource = MakeShared<FLodeRevision, ESPMode::ThreadSafe>(Record.CopyFromPath, Source);
	}
}

bool FLodeRevision::Get(FString& InOutFilename, EConcurrency::Type InConcurrency) const
{
	FLodeSourceControlModule* Module = FLodeSourceControlModule::GetIfLoaded();
	if (!Module)
	{
		return false;
	}
	return Module->GetProvider().FetchRevision(Record.DepotPath, Record.Rev, FPaths::GetExtension(Filename, true), InOutFilename);
}

bool FLodeRevision::GetAnnotated(TArray<FAnnotationLine>& OutLines) const
{
	return false;
}

bool FLodeRevision::GetAnnotated(FString& InOutFilename) const
{
	return false;
}

const FString& FLodeRevision::GetFilename() const
{
	return Filename;
}

int32 FLodeRevision::GetRevisionNumber() const
{
	return Record.Rev;
}

const FString& FLodeRevision::GetRevision() const
{
	return RevisionText;
}

const FString& FLodeRevision::GetDescription() const
{
	return Record.Message;
}

const FString& FLodeRevision::GetUserName() const
{
	return UserText;
}

const FString& FLodeRevision::GetClientSpec() const
{
	return Record.Branch;
}

const FString& FLodeRevision::GetAction() const
{
	return ActionText;
}

TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> FLodeRevision::GetBranchSource() const
{
	return BranchSource;
}

const FDateTime& FLodeRevision::GetDate() const
{
	return Record.Date;
}

int32 FLodeRevision::GetCheckInIdentifier() const
{
	return Record.Rev;
}

int32 FLodeRevision::GetFileSize() const
{
	return static_cast<int32>(FMath::Min<int64>(Record.Size, MAX_int32));
}
