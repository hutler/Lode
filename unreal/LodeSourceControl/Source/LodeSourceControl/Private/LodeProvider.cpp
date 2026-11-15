// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeProvider.h"

#include "Async/Async.h"
#include "Dom/JsonObject.h"
#include "HAL/FileManager.h"
#include "HAL/PlatformProcess.h"
#include "HAL/PlatformTime.h"
#include "ISourceControlModule.h"
#include "Logging/MessageLog.h"
#include "Misc/FileHelper.h"
#include "Misc/Paths.h"
#include "Misc/QueuedThreadPool.h"
#include "Policies/CondensedJsonPrintPolicy.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "SourceControlHelpers.h"
#include "SourceControlOperations.h"
#include "ScopedSourceControlProgress.h"

#if SOURCE_CONTROL_WITH_SLATE
#include "SLodeSettings.h"
#endif

#include "LodeLog.h"
#include "LodeOperations.h"
#include "LodePaths.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl"

DEFINE_LOG_CATEGORY(LogLodeSourceControl);

namespace
{
	const FName ProviderName(TEXT("Lode"));
	/** The most files a timed server refresh asks about. */
	constexpr int32 MaxServerRefreshFiles = 5000;
	/** Temporary diff files start with this, so Close can find and remove them. */
	const TCHAR* DiffPrefix = TEXT("Lode-r");
}

FLodeProvider::FLodeProvider() = default;

FLodeProvider::~FLodeProvider() = default;

void FLodeProvider::Init(bool bForceConnection)
{
	// Called several times at startup; finding lode and the workspace costs no request.
	Settings.Load();
	const FString Executable = FLodeRunner::ResolveExecutable(Settings.GetLodePath());
	bLodeFound = !Executable.IsEmpty() || (RunnerOverride.IsSet() && !RunnerOverride->Executable.IsEmpty());
	const FString Root = WorkspaceRootOverride.IsEmpty() ? LodePaths::FindWorkspaceRoot(FPaths::ProjectDir()) : LodePaths::Normalize(WorkspaceRootOverride);
	bWorkspaceFound = !Root.IsEmpty();
	if (bWorkspaceFound)
	{
		Workspace.Root = Root;
	}
	bEnabled = true;
	LoadShelfMap();

	if (!bLodeFound)
	{
		Problem = TEXT("The lode program was not found. Install Lode, or set the path to lode in the settings below.");
	}
	else if (!bWorkspaceFound)
	{
		Problem = FString::Printf(TEXT("This project is not inside a Lode workspace. Check out the depot with `lode checkout <server>/<depot>` so that %s is inside it, or create one with `lode init <server>/<depot> --template unreal`."), *FPaths::ConvertRelativePathToFull(FPaths::GetProjectFilePath()));
	}

	if (bForceConnection && bLodeFound && bWorkspaceFound && !bSignedIn)
	{
		Execute(ISourceControlOperation::Create<FConnect>(), FSourceControlChangelistPtr(), TArray<FString>(), EConcurrency::Asynchronous);
	}
}

void FLodeProvider::Close()
{
	for (FLodeCommand* Command : CommandQueue)
	{
		Command->Cancel->store(true);
	}
	// Commands on pool threads finish quickly once cancelled; wait so none outlives the provider.
	const double Deadline = FPlatformTime::Seconds() + 10.0;
	while (CommandQueue.Num() > 0 && FPlatformTime::Seconds() < Deadline)
	{
		for (int32 Index = CommandQueue.Num() - 1; Index >= 0; --Index)
		{
			FLodeCommand* Command = CommandQueue[Index];
			if (Command->bExecuteProcessed)
			{
				CommandQueue.RemoveAt(Index);
				if (Command->bAutoDelete)
				{
					delete Command;
				}
			}
		}
		FPlatformProcess::Sleep(0.01f);
	}

	// Temporary files made for diffs.
	TArray<FString> DiffFiles;
	IFileManager::Get().FindFiles(DiffFiles, *(FPaths::DiffDir() / (FString(DiffPrefix) + TEXT("*"))), true, false);
	for (const FString& File : DiffFiles)
	{
		IFileManager::Get().Delete(*(FPaths::DiffDir() / File), false, true, true);
	}

	StateCache.Empty();
	ChangelistsStateCache.Empty();
	LastAsked.Empty();
	bSignedIn = false;
	bEnabled = false;
}

const FName& FLodeProvider::GetName() const
{
	return ProviderName;
}

FText FLodeProvider::GetStatusText() const
{
	FFormatNamedArguments Args;
	Args.Add(TEXT("Server"), FText::FromString(Workspace.Server.IsEmpty() ? TEXT("?") : Workspace.Server));
	Args.Add(TEXT("Depot"), FText::FromString(Workspace.Depot.IsEmpty() ? TEXT("?") : Workspace.Depot));
	Args.Add(TEXT("Branch"), FText::FromString(Workspace.Branch.IsEmpty() ? TEXT("?") : Workspace.Branch));
	Args.Add(TEXT("Rev"), FText::AsNumber(Workspace.Rev));
	Args.Add(TEXT("User"), FText::FromString(Workspace.UserName.IsEmpty() ? TEXT("not signed in") : Workspace.UserName));
	Args.Add(TEXT("Root"), FText::FromString(Workspace.Root));
	Args.Add(TEXT("Version"), FText::FromString(Workspace.ClientVersion));
	FText Text = FText::Format(LOCTEXT("StatusText", "Lode {Version}\nServer: {Server}\nDepot: {Depot}\nBranch: {Branch} at r{Rev}\nUser: {User}\nWorkspace: {Root}"), Args);
	if (!Problem.IsEmpty())
	{
		Text = FText::Format(LOCTEXT("StatusTextProblem", "{0}\n\n{1}"), FText::FromString(Problem), Text);
	}
	return Text;
}

TMap<ISourceControlProvider::EStatus, FString> FLodeProvider::GetStatus() const
{
	TMap<EStatus, FString> Result;
	Result.Add(EStatus::Enabled, IsEnabled() ? TEXT("Yes") : TEXT("No"));
	Result.Add(EStatus::Connected, IsAvailable() ? TEXT("Yes") : TEXT("No"));
	Result.Add(EStatus::User, Workspace.UserName);
	Result.Add(EStatus::ScmVersion, Workspace.ClientVersion);
	Result.Add(EStatus::WorkspacePath, Workspace.Root);
	Result.Add(EStatus::Workspace, Workspace.Depot);
	Result.Add(EStatus::Branch, Workspace.Branch);
	Result.Add(EStatus::Changeset, FString::FromInt(Workspace.Rev));
	return Result;
}

bool FLodeProvider::IsEnabled() const
{
	return bEnabled && bLodeFound && bWorkspaceFound;
}

bool FLodeProvider::IsAvailable() const
{
	return IsEnabled() && bSignedIn;
}

FLodeRunnerConfig FLodeProvider::MakeRunnerConfig() const
{
	FLodeRunnerConfig Config;
	if (RunnerOverride.IsSet() && !RunnerOverride->Executable.IsEmpty())
	{
		Config = RunnerOverride.GetValue();
	}
	else
	{
		Config.Executable = FLodeRunner::ResolveExecutable(Settings.GetLodePath());
	}
	if (Config.WorkingDirectory.IsEmpty())
	{
		Config.WorkingDirectory = Workspace.Root.IsEmpty() ? FPaths::ConvertRelativePathToFull(FPaths::ProjectDir()) : Workspace.Root;
	}
	if (Config.TempDirectory.IsEmpty())
	{
		Config.TempDirectory = GetTempDirectory();
	}
	return Config;
}

FString FLodeProvider::GetTempDirectory() const
{
	return FPaths::ConvertRelativePathToFull(FPaths::ProjectIntermediateDir() / TEXT("LodeSourceControl"));
}

void FLodeProvider::ApplyWorkspace(const FLodeWorkspaceInfo& Info, bool bInLodeFound, bool bInWorkspaceFound, bool bInSignedIn, const FString& InProblem)
{
	const FString OldRoot = Workspace.Root;
	Workspace = Info;
	bLodeFound = bInLodeFound;
	bWorkspaceFound = bInWorkspaceFound;
	bSignedIn = bInSignedIn;
	Problem = InProblem;
	if (!OldRoot.IsEmpty() && OldRoot != Workspace.Root)
	{
		StateCache.Empty();
		ChangelistsStateCache.Empty();
	}
	NextServerRefresh = 0.0;
}

ECommandResult::Type FLodeProvider::GetState(const TArray<FString>& InFiles, TArray<FSourceControlStateRef>& OutState, EStateCacheUsage::Type InStateCacheUsage)
{
	if (!IsEnabled())
	{
		return ECommandResult::Failed;
	}

	TArray<FString> Files;
	Files.Reserve(InFiles.Num());
	for (const FString& File : SourceControlHelpers::AbsoluteFilenames(InFiles))
	{
		Files.Add(LodePaths::Normalize(File));
	}

	if (InStateCacheUsage == EStateCacheUsage::ForceUpdate)
	{
		Execute(ISourceControlOperation::Create<FUpdateStatus>(), Files);
	}

	const double Now = FPlatformTime::Seconds();
	for (const FString& File : Files)
	{
		OutState.Add(GetStateInternal(File));
		LastAsked.Add(File, Now);
	}
	return ECommandResult::Succeeded;
}

ECommandResult::Type FLodeProvider::GetState(const TArray<FSourceControlChangelistRef>& InChangelists, TArray<FSourceControlChangelistStateRef>& OutState, EStateCacheUsage::Type InStateCacheUsage)
{
	if (!IsEnabled())
	{
		return ECommandResult::Failed;
	}
	if (InStateCacheUsage == EStateCacheUsage::ForceUpdate)
	{
		TSharedRef<FUpdatePendingChangelistsStatus, ESPMode::ThreadSafe> Operation = ISourceControlOperation::Create<FUpdatePendingChangelistsStatus>();
		Operation->SetChangelistsToUpdate(InChangelists);
		ISourceControlProvider::Execute(Operation, EConcurrency::Synchronous);
	}
	for (const FSourceControlChangelistRef& Changelist : InChangelists)
	{
		const FLodeChangelist& LodeChangelist = static_cast<const FLodeChangelist&>(Changelist.Get());
		OutState.Add(GetChangelistStateInternal(LodeChangelist));
	}
	return ECommandResult::Succeeded;
}

TArray<FSourceControlStateRef> FLodeProvider::GetCachedStateByPredicate(TFunctionRef<bool(const FSourceControlStateRef&)> Predicate) const
{
	TArray<FSourceControlStateRef> Result;
	for (const TPair<FString, FLodeStateRef>& Item : StateCache)
	{
		const FSourceControlStateRef State = Item.Value;
		if (Predicate(State))
		{
			Result.Add(State);
		}
	}
	return Result;
}

FDelegateHandle FLodeProvider::RegisterSourceControlStateChanged_Handle(const FSourceControlStateChanged::FDelegate& SourceControlStateChanged)
{
	return OnSourceControlStateChanged.Add(SourceControlStateChanged);
}

void FLodeProvider::UnregisterSourceControlStateChanged_Handle(FDelegateHandle Handle)
{
	OnSourceControlStateChanged.Remove(Handle);
}

void FLodeProvider::RegisterWorker(const FName& InName, const FGetLodeWorker& InDelegate)
{
	WorkersMap.Add(InName, InDelegate);
}

TSharedPtr<ILodeWorker, ESPMode::ThreadSafe> FLodeProvider::CreateWorker(const FName& InOperationName) const
{
	if (const FGetLodeWorker* Factory = WorkersMap.Find(InOperationName))
	{
		return Factory->Execute();
	}
	return nullptr;
}

bool FLodeProvider::CanExecuteOperation(const FSourceControlOperationRef& InOperation) const
{
	return WorkersMap.Contains(InOperation->GetName());
}

ECommandResult::Type FLodeProvider::Execute(const FSourceControlOperationRef& InOperation, FSourceControlChangelistPtr InChangelist, const TArray<FString>& InFiles, EConcurrency::Type InConcurrency, const FSourceControlOperationComplete& InOperationCompleteDelegate)
{
	const FName OperationName = InOperation->GetName();
	if (!IsEnabled() && OperationName != TEXT("Connect"))
	{
		InOperation->AddErrorMessge(FText::FromString(Problem.IsEmpty() ? TEXT("Lode is not connected.") : Problem));
		InOperationCompleteDelegate.ExecuteIfBound(InOperation, ECommandResult::Failed);
		return ECommandResult::Failed;
	}

	const TSharedPtr<ILodeWorker, ESPMode::ThreadSafe> Worker = CreateWorker(OperationName);
	if (!Worker.IsValid())
	{
		const FText Message = FText::Format(LOCTEXT("UnsupportedOperation", "Lode does not support the '{0}' operation."), FText::FromName(OperationName));
		FMessageLog("SourceControl").Error(Message);
		InOperation->AddErrorMessge(Message);
		InOperationCompleteDelegate.ExecuteIfBound(InOperation, ECommandResult::Failed);
		return ECommandResult::Failed;
	}

	FLodeCommand* Command = new FLodeCommand(InOperation, Worker.ToSharedRef());
	Command->OperationCompleteDelegate = InOperationCompleteDelegate;
	Command->Workspace = Workspace;
	Command->RunnerConfig = MakeRunnerConfig();
	Command->bKeepLocksDefault = Settings.GetKeepLocksDefault();
	for (const FString& File : SourceControlHelpers::AbsoluteFilenames(InFiles))
	{
		const FString Normalized = LodePaths::Normalize(File);
		Command->Files.Add(Normalized);
		if (const FLodeStateRef* State = StateCache.Find(Normalized))
		{
			Command->KnownRecords.Add(Normalized, (*State)->GetRecord());
		}
	}
	if (InChangelist.IsValid())
	{
		Command->Changelist = static_cast<const FLodeChangelist&>(*InChangelist);
		// Workers that act on a whole changelist need its files and shelf.
		if (const FLodeChangelistStateRef* ChangelistState = ChangelistsStateCache.Find(Command->Changelist))
		{
			for (const FSourceControlStateRef& FileState : (*ChangelistState)->Files)
			{
				const FLodeState& LodeState = static_cast<const FLodeState&>(FileState.Get());
				Command->KnownRecords.Add(LodeState.GetFilename(), LodeState.GetRecord());
			}
		}
	}

	LodeWorkers::PrepareWorker(*Worker, *this, Command->Changelist);

	if (InConcurrency == EConcurrency::Synchronous)
	{
		Command->bAutoDelete = false;
		return ExecuteSynchronousCommand(*Command, InOperation->GetInProgressString());
	}
	Command->bAutoDelete = true;
	return IssueCommand(*Command);
}

ECommandResult::Type FLodeProvider::ExecuteSynchronousCommand(FLodeCommand& InCommand, const FText& Task)
{
	ECommandResult::Type Result = ECommandResult::Failed;
	{
		// The editor waits here, but lode runs on a pool thread and the progress window stays live
		// (with a Cancel button where the editor offers one).
		FScopedSourceControlProgress Progress(Task, FSimpleDelegate::CreateLambda([&InCommand]() { InCommand.Cancel->store(true); }));
		IssueCommand(InCommand);
		while (!InCommand.bExecuteProcessed)
		{
			Tick();
			Progress.Tick();
			FPlatformProcess::Sleep(0.01f);
		}
		// Finish this command (UpdateStates, messages, delegate) through the normal path.
		Tick();
		if (CommandQueue.Contains(&InCommand))
		{
			// Tick handles one finished command per call; make sure ours is done.
			while (CommandQueue.Contains(&InCommand))
			{
				Tick();
			}
		}
		Result = InCommand.Cancel->load() ? ECommandResult::Cancelled : (InCommand.bCommandSuccessful ? ECommandResult::Succeeded : ECommandResult::Failed);
	}
	check(!InCommand.bAutoDelete);
	delete &InCommand;
	return Result;
}

ECommandResult::Type FLodeProvider::IssueCommand(FLodeCommand& InCommand)
{
	if (GThreadPool != nullptr)
	{
		CommandQueue.Add(&InCommand);
		GThreadPool->AddQueuedWork(&InCommand);
		return ECommandResult::Succeeded;
	}

	// No thread pool (a commandlet): run here. This is the one case where lode runs on the
	// calling thread, so the runner's game-thread guard is relaxed by running on a task thread.
	const FText Message = LOCTEXT("NoThreads", "There are no threads to run the Lode command on.");
	InCommand.ErrorMessages.Add(Message);
	InCommand.bCommandSuccessful = false;
	InCommand.bExecuteProcessed = true;
	const ECommandResult::Type Result = InCommand.ReturnResults();
	if (InCommand.bAutoDelete)
	{
		delete &InCommand;
	}
	return Result;
}

bool FLodeProvider::CanCancelOperation(const FSourceControlOperationRef& InOperation) const
{
	for (const FLodeCommand* Command : CommandQueue)
	{
		if (Command->Operation == InOperation)
		{
			return true;
		}
	}
	return false;
}

void FLodeProvider::CancelOperation(const FSourceControlOperationRef& InOperation)
{
	for (FLodeCommand* Command : CommandQueue)
	{
		if (Command->Operation == InOperation)
		{
			Command->Cancel->store(true);
		}
	}
}

TOptional<bool> FLodeProvider::IsAtLatestRevision() const
{
	return TOptional<bool>();
}

TOptional<int> FLodeProvider::GetNumLocalChanges() const
{
	int32 Count = 0;
	for (const TPair<FString, FLodeStateRef>& Item : StateCache)
	{
		Count += Item.Value->HasLocalChange() ? 1 : 0;
	}
	return Count;
}

void FLodeProvider::OutputCommandMessages(const FLodeCommand& InCommand) const
{
	FMessageLog SourceControlLog("SourceControl");
	for (const FText& Message : InCommand.ErrorMessages)
	{
		SourceControlLog.Error(Message);
	}
	for (const FText& Message : InCommand.InfoMessages)
	{
		SourceControlLog.Info(Message);
	}
}

void FLodeProvider::Tick()
{
	bool bStatesUpdated = false;
	for (int32 Index = 0; Index < CommandQueue.Num(); ++Index)
	{
		FLodeCommand& Command = *CommandQueue[Index];
		if (!Command.bExecuteProcessed)
		{
			continue;
		}
		CommandQueue.RemoveAt(Index);
		bStatesUpdated |= Command.Worker->UpdateStates(*this);
		OutputCommandMessages(Command);
		UE_LOG(LogLodeSourceControl, Verbose, TEXT("%s of %d file(s) done in %.3fs"), *Command.Operation->GetName().ToString(), Command.Files.Num(), FPlatformTime::Seconds() - Command.StartTime);
		Command.ReturnResults();
		if (Command.Worker->GetName() == FName(TEXT("LodeRefreshServer")))
		{
			bServerRefreshInFlight = false;
		}
		if (Command.bAutoDelete)
		{
			delete &Command;
		}
		// One per tick: the delegate may have started another command and changed the queue.
		break;
	}

	if (bStatesUpdated)
	{
		OnSourceControlStateChanged.Broadcast();
	}

	QueueServerRefresh();
}

void FLodeProvider::QueueServerRefresh()
{
	if (!IsAvailable() || bServerRefreshInFlight)
	{
		return;
	}
	const double Now = FPlatformTime::Seconds();
	if (Now < NextServerRefresh)
	{
		return;
	}
	const double Interval = Settings.GetServerRefreshSeconds();
	NextServerRefresh = Now + Interval;

	// The files the editor asked about lately are the ones it shows.
	TArray<FString> Files;
	for (auto It = LastAsked.CreateIterator(); It; ++It)
	{
		if (Now - It.Value() > Interval * 2.0)
		{
			It.RemoveCurrent();
		}
		else if (Files.Num() < MaxServerRefreshFiles)
		{
			Files.Add(It.Key());
		}
	}
	if (Files.Num() == 0)
	{
		return;
	}
	bServerRefreshInFlight = true;
	Execute(ISourceControlOperation::Create<FLodeRefreshServer>(), FSourceControlChangelistPtr(), Files, EConcurrency::Asynchronous);
}

void FLodeProvider::WaitForCommands(double TimeoutSeconds)
{
	const double Deadline = FPlatformTime::Seconds() + TimeoutSeconds;
	while (CommandQueue.Num() > 0 && FPlatformTime::Seconds() < Deadline)
	{
		Tick();
		FPlatformProcess::Sleep(0.01f);
	}
}

TArray<FSourceControlChangelistRef> FLodeProvider::GetChangelists(EStateCacheUsage::Type InStateCacheUsage)
{
	if (!IsEnabled())
	{
		return {};
	}
	if (InStateCacheUsage == EStateCacheUsage::ForceUpdate)
	{
		TSharedRef<FUpdatePendingChangelistsStatus, ESPMode::ThreadSafe> Operation = ISourceControlOperation::Create<FUpdatePendingChangelistsStatus>();
		Operation->SetUpdateAllChangelists(true);
		ISourceControlProvider::Execute(Operation, EConcurrency::Synchronous);
	}
	TArray<FSourceControlChangelistRef> Changelists;
	for (const TPair<FLodeChangelist, FLodeChangelistStateRef>& Item : ChangelistsStateCache)
	{
		Changelists.Add(MakeShared<FLodeChangelist, ESPMode::ThreadSafe>(Item.Key));
	}
	return Changelists;
}

FLodeStateRef FLodeProvider::GetStateInternal(const FString& Filename)
{
	if (FLodeStateRef* State = StateCache.Find(Filename))
	{
		return *State;
	}
	FLodeStateRef State = MakeShared<FLodeState, ESPMode::ThreadSafe>(Filename);
	StateCache.Add(Filename, State);
	return State;
}

FLodeChangelistStateRef FLodeProvider::GetChangelistStateInternal(const FLodeChangelist& Changelist)
{
	if (FLodeChangelistStateRef* State = ChangelistsStateCache.Find(Changelist))
	{
		return *State;
	}
	FLodeChangelistStateRef State = MakeShared<FLodeChangelistState, ESPMode::ThreadSafe>(Changelist);
	ChangelistsStateCache.Add(Changelist, State);
	return State;
}

TSharedPtr<FLodeChangelistState, ESPMode::ThreadSafe> FLodeProvider::FindChangelistState(const FLodeChangelist& Changelist) const
{
	if (const FLodeChangelistStateRef* State = ChangelistsStateCache.Find(Changelist))
	{
		return *State;
	}
	return nullptr;
}

bool FLodeProvider::RemoveChangelistFromCache(const FLodeChangelist& Changelist)
{
	return ChangelistsStateCache.Remove(Changelist) > 0;
}

bool FLodeProvider::ApplyRecords(const TArray<FLodeFileRecord>& Records)
{
	bool bChanged = false;
	for (const FLodeFileRecord& Record : Records)
	{
		if (Record.Filename.IsEmpty())
		{
			continue;
		}
		FLodeStateRef State = GetStateInternal(Record.Filename);
		const FLodeFileRecord Before = State->GetRecord();
		State->SetRecord(Record);
		const FLodeFileRecord& After = State->GetRecord();
		bChanged |= Before.Status != After.Status || Before.bLockedHere != After.bLockedHere || Before.LockOwner != After.LockOwner
			|| Before.bNewerAtHead != After.bNewerAtHead || Before.Changelist != After.Changelist || Before.bNeedsLock != After.bNeedsLock;

		// Keep changelist membership in step with status.
		if (Before.Changelist != After.Changelist || Before.Status != After.Status)
		{
			const bool bInChangelist = State->HasLocalChange();
			const FLodeChangelist Target = After.Changelist.IsEmpty() ? FLodeChangelist::Default() : FLodeChangelist(After.Changelist);
			for (TPair<FLodeChangelist, FLodeChangelistStateRef>& Item : ChangelistsStateCache)
			{
				Item.Value->Files.RemoveAll([&Record](const FSourceControlStateRef& FileState) { return FileState->GetFilename() == Record.Filename; });
			}
			if (bInChangelist)
			{
				GetChangelistStateInternal(Target)->Files.Add(State);
			}
		}
	}
	return bChanged;
}

bool FLodeProvider::ApplyLocks(const TArray<FLodeLockRecord>& Locks, const TArray<FString>& Scope)
{
	TMap<FString, const FLodeLockRecord*> ByFilename;
	for (const FLodeLockRecord& Lock : Locks)
	{
		ByFilename.Add(Lock.Filename, &Lock);
	}
	bool bChanged = false;
	for (TPair<FString, FLodeStateRef>& Item : StateCache)
	{
		const bool bInScope = Scope.Num() == 0 || Scope.ContainsByPredicate([&Item](const FString& Folder) { return Item.Key.StartsWith(Folder); });
		if (!bInScope)
		{
			continue;
		}
		const FLodeFileRecord& Record = Item.Value->GetRecord();
		const FLodeLockRecord* const* Lock = ByFilename.Find(Item.Key);
		const FString Owner = Lock ? (*Lock)->Owner : FString();
		const bool bMine = Lock && (*Lock)->bMine;
		if (Record.LockOwner != (bMine ? FString() : Owner) || Record.bLockedHere != bMine)
		{
			Item.Value->SetLockOwner(Owner, bMine);
			bChanged = true;
		}
	}
	return bChanged;
}

int32 FLodeProvider::GetShelfForChangelist(const FString& ChangelistName, int32* OutVersion) const
{
	if (const TPair<int32, int32>* Shelf = ShelfByChangelist.Find(ChangelistName))
	{
		if (OutVersion)
		{
			*OutVersion = Shelf->Value;
		}
		return Shelf->Key;
	}
	return 0;
}

void FLodeProvider::SetShelfForChangelist(const FString& ChangelistName, int32 ShelfId, int32 Version)
{
	if (ShelfId > 0)
	{
		ShelfByChangelist.Add(ChangelistName, TPair<int32, int32>(ShelfId, Version));
	}
	else
	{
		ShelfByChangelist.Remove(ChangelistName);
	}
	SaveShelfMap();
}

namespace
{
	FString ShelfMapPath()
	{
		return FPaths::ProjectSavedDir() / TEXT("LodeSourceControl") / TEXT("Shelves.json");
	}
}

void FLodeProvider::LoadShelfMap()
{
	ShelfByChangelist.Reset();
	FString Text;
	if (!FFileHelper::LoadFileToString(Text, *ShelfMapPath()))
	{
		return;
	}
	TSharedPtr<FJsonObject> Json;
	const TSharedRef<TJsonReader<>> Reader = TJsonReaderFactory<>::Create(Text);
	if (!FJsonSerializer::Deserialize(Reader, Json) || !Json.IsValid())
	{
		return;
	}
	for (const TPair<FString, TSharedPtr<FJsonValue>>& Field : Json->Values)
	{
		const TSharedPtr<FJsonObject>* Entry = nullptr;
		if (Field.Value.IsValid() && Field.Value->TryGetObject(Entry) && Entry)
		{
			int32 Id = 0;
			int32 Version = 0;
			(*Entry)->TryGetNumberField(TEXT("shelf"), Id);
			(*Entry)->TryGetNumberField(TEXT("version"), Version);
			if (Id > 0)
			{
				ShelfByChangelist.Add(Field.Key, TPair<int32, int32>(Id, Version));
			}
		}
	}
}

void FLodeProvider::SaveShelfMap() const
{
	TSharedRef<FJsonObject> Json = MakeShared<FJsonObject>();
	for (const TPair<FString, TPair<int32, int32>>& Item : ShelfByChangelist)
	{
		TSharedRef<FJsonObject> Entry = MakeShared<FJsonObject>();
		Entry->SetNumberField(TEXT("shelf"), Item.Value.Key);
		Entry->SetNumberField(TEXT("version"), Item.Value.Value);
		Json->SetObjectField(Item.Key, Entry);
	}
	FString Text;
	const TSharedRef<TJsonWriter<>> Writer = TJsonWriterFactory<>::Create(&Text);
	FJsonSerializer::Serialize(Json, Writer);
	FFileHelper::SaveStringToFile(Text, *ShelfMapPath(), FFileHelper::EEncodingOptions::ForceUTF8WithoutBOM);
}

bool FLodeProvider::FetchRevision(const FString& DepotPath, int32 Rev, const FString& Extension, FString& InOutFilename)
{
	if (InOutFilename.IsEmpty())
	{
		IFileManager::Get().MakeDirectory(*FPaths::DiffDir(), true);
		InOutFilename = FPaths::ConvertRelativePathToFull(FPaths::DiffDir() / FString::Printf(TEXT("%s%d-%s"), DiffPrefix, Rev, *FPaths::GetCleanFilename(DepotPath)));
	}
	if (FPaths::FileExists(InOutFilename))
	{
		return true; // a revision never changes, so an earlier copy is good
	}

	FLodeInvocation Invocation{ TEXT("cat"), FString::Printf(TEXT("%s@%d"), *DepotPath, Rev), TEXT("-o"), InOutFilename };
	const FLodeRunnerConfig Config = MakeRunnerConfig();
	TFuture<FLodeResult> Future = Async(EAsyncExecution::ThreadPool, [Config, Invocation]() { return FLodeRunner(Config).Run(Invocation); });

	if (IsInGameThread())
	{
		FScopedSourceControlProgress Progress(LOCTEXT("FetchingRevision", "Getting the file from Lode..."));
		while (!Future.IsReady())
		{
			Progress.Tick();
			FPlatformProcess::Sleep(0.01f);
		}
	}
	const FLodeResult Result = Future.Get();
	if (!Result.IsOk())
	{
		FMessageLog("SourceControl").Error(FText::FromString(Result.Describe()));
		IFileManager::Get().Delete(*InOutFilename, false, true, true);
		return false;
	}
	return FPaths::FileExists(InOutFilename);
}

#if SOURCE_CONTROL_WITH_SLATE
TSharedRef<SWidget> FLodeProvider::MakeSettingsWidget() const
{
	return SNew(SLodeSettings);
}
#endif

#undef LOCTEXT_NAMESPACE
