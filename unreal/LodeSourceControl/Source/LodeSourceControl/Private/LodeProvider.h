// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlProvider.h"
#include "ISourceControlChangelistState.h"

#include "LodeChangelist.h"
#include "LodeCommand.h"
#include "LodeSettings.h"
#include "LodeState.h"
#include "LodeTypes.h"

DECLARE_DELEGATE_RetVal(FLodeWorkerRef, FGetLodeWorker)

/** Makes Lode a revision control provider. All public methods run on the game thread unless they say otherwise. */
class FLodeProvider : public ISourceControlProvider
{
public:
	FLodeProvider();
	virtual ~FLodeProvider() override;

	// ISourceControlProvider
	virtual void Init(bool bForceConnection = true) override;
	virtual void Close() override;
	virtual const FName& GetName() const override;
	virtual FText GetStatusText() const override;
	virtual TMap<EStatus, FString> GetStatus() const override;
	virtual bool IsEnabled() const override;
	virtual bool IsAvailable() const override;
	virtual bool QueryStateBranchConfig(const FString& ConfigSrc, const FString& ConfigDest) override { return false; }
	virtual void RegisterStateBranches(const TArray<FString>& BranchNames, const FString& ContentRoot) override {}
	virtual int32 GetStateBranchIndex(const FString& BranchName) const override { return INDEX_NONE; }
	// Added to the interface in UE 5.7; declared without override so 5.4 to 5.6 compile too.
	virtual bool GetStateBranchAtIndex(int32 BranchIndex, FString& OutBranchName) const { return false; }
	virtual ECommandResult::Type GetState(const TArray<FString>& InFiles, TArray<FSourceControlStateRef>& OutState, EStateCacheUsage::Type InStateCacheUsage) override;
	virtual ECommandResult::Type GetState(const TArray<FSourceControlChangelistRef>& InChangelists, TArray<FSourceControlChangelistStateRef>& OutState, EStateCacheUsage::Type InStateCacheUsage) override;
	virtual TArray<FSourceControlStateRef> GetCachedStateByPredicate(TFunctionRef<bool(const FSourceControlStateRef&)> Predicate) const override;
	virtual FDelegateHandle RegisterSourceControlStateChanged_Handle(const FSourceControlStateChanged::FDelegate& SourceControlStateChanged) override;
	virtual void UnregisterSourceControlStateChanged_Handle(FDelegateHandle Handle) override;
	virtual ECommandResult::Type Execute(const FSourceControlOperationRef& InOperation, FSourceControlChangelistPtr InChangelist, const TArray<FString>& InFiles, EConcurrency::Type InConcurrency = EConcurrency::Synchronous, const FSourceControlOperationComplete& InOperationCompleteDelegate = FSourceControlOperationComplete()) override;
	virtual bool CanExecuteOperation(const FSourceControlOperationRef& InOperation) const override;
	virtual bool CanCancelOperation(const FSourceControlOperationRef& InOperation) const override;
	virtual void CancelOperation(const FSourceControlOperationRef& InOperation) override;
	virtual bool UsesLocalReadOnlyState() const override { return true; }
	virtual bool UsesChangelists() const override { return true; }
	virtual bool UsesUncontrolledChangelists() const override { return false; }
	virtual bool UsesCheckout() const override { return true; }
	virtual bool UsesFileRevisions() const override { return false; }
	virtual bool UsesSnapshots() const override { return false; }
	virtual bool AllowsDiffAgainstDepot() const override { return true; }
	virtual TOptional<bool> IsAtLatestRevision() const override;
	virtual TOptional<int> GetNumLocalChanges() const override;
	virtual void Tick() override;
	virtual TArray<TSharedRef<class ISourceControlLabel>> GetLabels(const FString& InMatchingSpec) const override { return {}; }
	virtual TArray<FSourceControlChangelistRef> GetChangelists(EStateCacheUsage::Type InStateCacheUsage) override;
#if SOURCE_CONTROL_WITH_SLATE
	virtual TSharedRef<class SWidget> MakeSettingsWidget() const override;
#endif
	using ISourceControlProvider::Execute;

	// For workers and the settings panel (game thread).
	void RegisterWorker(const FName& InName, const FGetLodeWorker& InDelegate);
	FLodeStateRef GetStateInternal(const FString& Filename);
	FLodeChangelistStateRef GetChangelistStateInternal(const FLodeChangelist& Changelist);
	bool RemoveChangelistFromCache(const FLodeChangelist& Changelist);
	TSharedPtr<FLodeChangelistState, ESPMode::ThreadSafe> FindChangelistState(const FLodeChangelist& Changelist) const;
	/** Puts records into the cache. Returns true when anything changed. */
	bool ApplyRecords(const TArray<FLodeFileRecord>& Records);
	/** Sets lock holders from `lode locks` for files under the given folders. */
	bool ApplyLocks(const TArray<FLodeLockRecord>& Locks, const TArray<FString>& Scope);
	void ApplyWorkspace(const FLodeWorkspaceInfo& Info, bool bInLodeFound, bool bInWorkspaceFound, bool bInSignedIn, const FString& InProblem);

	FLodeSettings& AccessSettings() { return Settings; }
	const FLodeSettings& GetSettings() const { return Settings; }
	const FLodeWorkspaceInfo& GetWorkspace() const { return Workspace; }
	bool IsLodeFound() const { return bLodeFound; }
	bool IsWorkspaceFound() const { return bWorkspaceFound; }
	bool IsSignedIn() const { return bSignedIn; }
	const FString& GetProblem() const { return Problem; }

	/** The runner settings for this project: lode's path, the workspace root, the temporary folder. */
	FLodeRunnerConfig MakeRunnerConfig() const;

	/** Lets tests run the plugin against the fake lode. Empty Executable restores the setting. */
	void SetRunnerOverride(const FLodeRunnerConfig& InConfig) { RunnerOverride = InConfig; }
	/** Lets tests use a workspace other than the one holding the project. */
	void SetWorkspaceRootOverride(const FString& InRoot) { WorkspaceRootOverride = InRoot; }
	/** Runs commands until none is left (tests). */
	void WaitForCommands(double TimeoutSeconds);

	/** Shelf ids remembered per changelist (kept in Saved/LodeSourceControl/Shelves.json). */
	int32 GetShelfForChangelist(const FString& ChangelistName, int32* OutVersion = nullptr) const;
	void SetShelfForChangelist(const FString& ChangelistName, int32 ShelfId, int32 Version);

	/** Fetches a file at a revision into a temporary file for a diff; blocks with a progress dialog. */
	bool FetchRevision(const FString& DepotPath, int32 Rev, const FString& Extension, FString& InOutFilename);

	/** Folder for temporary files made for diffs; emptied when the editor closes. */
	FString GetTempDirectory() const;

	/** Asks for a refresh of server information soon (after a lock elsewhere, say). */
	void RequestServerRefresh() { NextServerRefresh = 0.0; }

private:
	TSharedPtr<ILodeWorker, ESPMode::ThreadSafe> CreateWorker(const FName& InOperationName) const;
	ECommandResult::Type ExecuteSynchronousCommand(FLodeCommand& InCommand, const FText& Task);
	ECommandResult::Type IssueCommand(FLodeCommand& InCommand);
	void OutputCommandMessages(const FLodeCommand& InCommand) const;
	void LoadShelfMap();
	void SaveShelfMap() const;
	void QueueServerRefresh();

	FLodeSettings Settings;
	FLodeWorkspaceInfo Workspace;
	bool bLodeFound = false;
	bool bWorkspaceFound = false;
	bool bSignedIn = false;
	bool bEnabled = false;
	FString Problem;
	TOptional<FLodeRunnerConfig> RunnerOverride;
	FString WorkspaceRootOverride;

	TMap<FString, FLodeStateRef> StateCache;
	TMap<FLodeChangelist, FLodeChangelistStateRef> ChangelistsStateCache;
	/** When the editor last asked about a file: the files it shows. */
	TMap<FString, double> LastAsked;
	double NextServerRefresh = 0.0;
	bool bServerRefreshInFlight = false;

	TMap<FName, FGetLodeWorker> WorkersMap;
	TArray<FLodeCommand*> CommandQueue;
	FSourceControlStateChanged OnSourceControlStateChanged;

	TMap<FString, TPair<int32, int32>> ShelfByChangelist;
};
