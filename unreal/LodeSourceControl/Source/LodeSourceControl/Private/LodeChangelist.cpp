// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeChangelist.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl.Changelist"

const FString FLodeChangelist::DefaultName(TEXT("default"));

FName FLodeChangelistState::GetIconName() const
{
	return ShelvedFiles.Num() > 0 && Files.Num() == 0 ? FName("SourceControl.ShelvedChangelist") : FName("SourceControl.Changelist");
}

FName FLodeChangelistState::GetSmallIconName() const
{
	return GetIconName();
}

FText FLodeChangelistState::GetDisplayText() const
{
	return Changelist.IsDefault() ? LOCTEXT("Default", "Default") : FText::FromString(Changelist.GetName());
}

FText FLodeChangelistState::GetDescriptionText() const
{
	return FText::FromString(Description);
}

FText FLodeChangelistState::GetDisplayTooltip() const
{
	if (ShelfId > 0)
	{
		return FText::Format(LOCTEXT("TooltipShelf", "{0}\nShelved as shelf {1} (version {2})"), GetDisplayText(), FText::AsNumber(ShelfId), FText::AsNumber(ShelfVersion));
	}
	return GetDisplayText();
}

const FDateTime& FLodeChangelistState::GetTimeStamp() const
{
	return TimeStamp;
}

const TArray<FSourceControlStateRef> FLodeChangelistState::GetFilesStates() const
{
	return Files;
}

int32 FLodeChangelistState::GetFilesStatesNum() const
{
	return Files.Num();
}

const TArray<FSourceControlStateRef> FLodeChangelistState::GetShelvedFilesStates() const
{
	return ShelvedFiles;
}

int32 FLodeChangelistState::GetShelvedFilesStatesNum() const
{
	return ShelvedFiles.Num();
}

FSourceControlChangelistRef FLodeChangelistState::GetChangelist() const
{
	return MakeShared<FLodeChangelist, ESPMode::ThreadSafe>(Changelist);
}

#undef LOCTEXT_NAMESPACE
