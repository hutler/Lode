// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlProvider.h"
#include "Misc/IQueuedWork.h"

#include "LodeChangelist.h"
#include "LodeRunner.h"
#include "LodeTypes.h"

class ILodeWorker;
class FLodeProvider;
using FLodeWorkerRef = TSharedRef<ILodeWorker, ESPMode::ThreadSafe>;

/**
 * One editor operation in flight. Created on the game thread with a snapshot of what the
 * worker needs (workspace, runner settings, the files' last known records), run on a pool
 * thread, and finished on the game thread by the provider's Tick.
 */
class FLodeCommand : public IQueuedWork
{
public:
	FLodeCommand(const FSourceControlOperationRef& InOperation, const FLodeWorkerRef& InWorker);

	/** Runs the worker. Called on a pool thread (asynchronous) or by the provider's wait loop. */
	bool DoWork();
	virtual void DoThreadedWork() override;
	virtual void Abandon() override;

	/** Copies messages to the operation and calls the completion delegate. Game thread. */
	ECommandResult::Type ReturnResults();

	/** Runs lode with this command's runner and cancel flag; failures are added to ErrorMessages. */
	FLodeResult Run(const FLodeInvocation& Invocation, bool bReportFailure = true);

	/** Like Run, splitting long path lists for commands without --paths-from. */
	bool RunAll(const FLodeInvocation& Invocation, TArray<FLodeResult>* OutResults = nullptr);

	/** Adds a failure in plain words, naming the files when lode named them. */
	void AddFailure(const FLodeResult& Result);

	/** The last known record of a file, or a blank one. */
	FLodeFileRecord KnownRecord(const FString& Filename) const;

	FSourceControlOperationRef Operation;
	FLodeWorkerRef Worker;
	FSourceControlOperationComplete OperationCompleteDelegate;

	/** Absolute, normalized filenames the editor named. */
	TArray<FString> Files;
	FLodeChangelist Changelist;
	FLodeWorkspaceInfo Workspace;
	FLodeRunnerConfig RunnerConfig;
	TMap<FString, FLodeFileRecord> KnownRecords;
	bool bKeepLocksDefault = false;

	TSharedRef<FLodeCancelFlag, ESPMode::ThreadSafe> Cancel;
	std::atomic<bool> bExecuteProcessed { false };
	bool bCommandSuccessful = false;
	bool bAutoDelete = true;
	EConcurrency::Type Concurrency = EConcurrency::Synchronous;
	double StartTime = 0.0;

	TArray<FText> InfoMessages;
	TArray<FText> ErrorMessages;
};

/** One kind of editor operation. A new worker is made for each command. */
class ILodeWorker
{
public:
	virtual ~ILodeWorker() = default;
	virtual FName GetName() const = 0;
	/** Pool thread: run lode, keep results in members. Never touch editor state here. */
	virtual bool Execute(FLodeCommand& Command) = 0;
	/** Game thread: move results into the provider's caches. Returns true when states changed. */
	virtual bool UpdateStates(FLodeProvider& Provider) const = 0;
};
