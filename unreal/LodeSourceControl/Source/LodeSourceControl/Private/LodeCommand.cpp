// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeCommand.h"

#include "HAL/PlatformTime.h"
#include "Dom/JsonValue.h"
#include "ISourceControlOperation.h"

#include "LodeJsonFields.h"
#include "LodeLog.h"
#include "LodePaths.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl"

FLodeCommand::FLodeCommand(const FSourceControlOperationRef& InOperation, const FLodeWorkerRef& InWorker)
	: Operation(InOperation)
	, Worker(InWorker)
	, Cancel(MakeShared<FLodeCancelFlag, ESPMode::ThreadSafe>(false))
	, StartTime(FPlatformTime::Seconds())
{
}

bool FLodeCommand::DoWork()
{
	bCommandSuccessful = Worker->Execute(*this);
	bExecuteProcessed = true;
	return bCommandSuccessful;
}

void FLodeCommand::DoThreadedWork()
{
	Concurrency = EConcurrency::Asynchronous;
	DoWork();
}

void FLodeCommand::Abandon()
{
	bExecuteProcessed = true;
}

ECommandResult::Type FLodeCommand::ReturnResults()
{
	for (const FText& Message : InfoMessages)
	{
		Operation->AddInfoMessge(Message);
	}
	for (const FText& Message : ErrorMessages)
	{
		Operation->AddErrorMessge(Message);
	}

	ECommandResult::Type Result = ECommandResult::Failed;
	if (Cancel->load())
	{
		Result = ECommandResult::Cancelled;
	}
	else if (bCommandSuccessful)
	{
		Result = ECommandResult::Succeeded;
	}
	OperationCompleteDelegate.ExecuteIfBound(Operation, Result);
	return Result;
}

FLodeResult FLodeCommand::Run(const FLodeInvocation& Invocation, bool bReportFailure)
{
	FLodeInvocation WithCancel = Invocation;
	WithCancel.Cancel = Cancel;
	const FLodeResult Result = FLodeRunner(RunnerConfig).Run(WithCancel);
	if (bReportFailure && !Result.IsOk() && Result.Outcome != ELodeOutcome::Cancelled)
	{
		AddFailure(Result);
	}
	return Result;
}

bool FLodeCommand::RunAll(const FLodeInvocation& Invocation, TArray<FLodeResult>* OutResults)
{
	FLodeInvocation WithCancel = Invocation;
	WithCancel.Cancel = Cancel;
	TArray<FLodeResult> Results = FLodeRunner(RunnerConfig).RunBatched(WithCancel);
	bool bAllOk = Results.Num() > 0;
	for (const FLodeResult& Result : Results)
	{
		if (!Result.IsOk())
		{
			bAllOk = false;
			if (Result.Outcome != ELodeOutcome::Cancelled)
			{
				AddFailure(Result);
			}
		}
	}
	if (OutResults)
	{
		*OutResults = MoveTemp(Results);
	}
	return bAllOk;
}

void FLodeCommand::AddFailure(const FLodeResult& Result)
{
	FString Text;
	const FString& Code = Result.Error.Code;
	if (Code == LodeErrorCode::OutOfDate)
	{
		Text = TEXT("Newer versions of these files are on the server. Sync (Get Latest), then submit again.");
	}
	else if (Code == LodeErrorCode::Locked)
	{
		Text = TEXT("Someone else holds the lock on these files, so you cannot change them now.");
		if (Result.Error.Details.IsValid())
		{
			const TArray<TSharedPtr<FJsonValue>>* Locks = nullptr;
			if (Result.Error.Details->TryGetArrayField(LodeJson::Locks, Locks) && Locks)
			{
				TArray<FString> Holders;
				for (const TSharedPtr<FJsonValue>& Lock : *Locks)
				{
					const TSharedPtr<FJsonObject>* LockObject = nullptr;
					const TSharedPtr<FJsonObject>* OwnerObject = nullptr;
					FString OwnerName;
					FString Path;
					if (Lock.IsValid() && Lock->TryGetObject(LockObject) && LockObject
						&& (*LockObject)->TryGetObjectField(LodeJson::Owner, OwnerObject) && OwnerObject
						&& (*OwnerObject)->TryGetStringField(LodeJson::Name, OwnerName))
					{
						(*LockObject)->TryGetStringField(LodeJson::Path, Path);
						Holders.Add(FString::Printf(TEXT("%s (held by %s)"), *Path, *OwnerName));
					}
				}
				if (Holders.Num() > 0)
				{
					Text = FString::Printf(TEXT("Locked by someone else: %s."), *FString::Join(Holders, TEXT(", ")));
				}
			}
		}
	}
	else if (Code == LodeErrorCode::LockRequired)
	{
		Text = TEXT("These files need a lock before they can be submitted. Check them out (lock them) first.");
	}
	else if (Code == LodeErrorCode::Conflicted)
	{
		Text = TEXT("Some files are in conflict. Resolve them before you submit.");
	}
	else if (Code == LodeErrorCode::NothingToCommit)
	{
		Text = TEXT("There is nothing to submit: the files have no changes.");
	}
	else if (Code == LodeErrorCode::ModuleDisabled)
	{
		Text = TEXT("The Lode server has this feature turned off (locks or shelves). Ask the server's administrator.");
	}
	else if (Code == LodeErrorCode::PermissionDenied)
	{
		Text = FString::Printf(TEXT("Not allowed: %s"), *Result.Error.Message);
	}
	else if (Code == LodeErrorCode::WorkspaceInUse)
	{
		Text = TEXT("Another lode command is working in this workspace. Try again when it finishes.");
	}
	else
	{
		Text = Result.Describe();
	}

	if (Result.Error.Paths.Num() > 0 && Code != LodeErrorCode::Locked)
	{
		Text += FString::Printf(TEXT(" Files: %s."), *FString::Join(Result.Error.Paths, TEXT(", ")));
	}
	if (!Code.IsEmpty() && Result.Error.Message.Len() > 0 && !Text.Contains(Result.Error.Message))
	{
		Text += FString::Printf(TEXT(" (lode: %s)"), *Result.Error.Message);
	}
	ErrorMessages.Add(FText::FromString(Text));
}

FLodeFileRecord FLodeCommand::KnownRecord(const FString& Filename) const
{
	if (const FLodeFileRecord* Record = KnownRecords.Find(Filename))
	{
		return *Record;
	}
	FLodeFileRecord Blank;
	Blank.Filename = Filename;
	Blank.DepotPath = LodePaths::ToDepotPath(Workspace.Root, Filename);
	return Blank;
}

#undef LOCTEXT_NAMESPACE
