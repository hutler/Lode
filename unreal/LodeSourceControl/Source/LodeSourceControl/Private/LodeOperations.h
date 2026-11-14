// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "ISourceControlOperation.h"

#include "LodeCommand.h"
#include "LodeTypes.h"

class FLodeProvider;

#define LOCTEXT_NAMESPACE "LodeSourceControl"

/** Asks the server who holds locks and which files are newer, for the files the editor shows. */
class FLodeRefreshServer : public ISourceControlOperation
{
public:
	virtual FName GetName() const override { return "LodeRefreshServer"; }
	virtual FText GetInProgressString() const override { return LOCTEXT("RefreshServer", "Asking the Lode server for locks and newer files..."); }
};

/** Signs in with `lode login`. The secret lives only in memory and is cleared once used. */
class FLodeLogin : public ISourceControlOperation
{
public:
	enum class EMethod : uint8 { Token, Password, SingleSignOn };

	virtual FName GetName() const override { return "LodeLogin"; }
	virtual FText GetInProgressString() const override { return LOCTEXT("Login", "Signing in to Lode..."); }

	EMethod Method = EMethod::Token;
	FString UserName;
	/** Token or password. Never logged, never saved. */
	FString Secret;
	/** Filled on success. */
	FString SignedInAs;
};

/** Settles conflicted files to one side, for binary assets that cannot be merged. */
class FLodeResolve : public ISourceControlOperation
{
public:
	virtual FName GetName() const override { return "LodeResolve"; }
	virtual FText GetInProgressString() const override { return LOCTEXT("Resolve", "Resolving conflicts..."); }

	/** true: take the other side (theirs); false: keep this workspace's file (mine). */
	bool bTakeTheirs = false;
};

/** Lists open shelves, everyone's, for the Lode Shelves window. */
class FLodeListShelves : public ISourceControlOperation
{
public:
	virtual FName GetName() const override { return "LodeListShelves"; }
	virtual FText GetInProgressString() const override { return LOCTEXT("ListShelves", "Listing shelves..."); }

	/** Filled on success, newest first, each with its files. */
	TArray<FLodeShelfRecord> Shelves;
};

/** Unshelves one shelf by id into this workspace (anyone's shelf). */
class FLodeUnshelveShelf : public ISourceControlOperation
{
public:
	virtual FName GetName() const override { return "LodeUnshelveShelf"; }
	virtual FText GetInProgressString() const override { return LOCTEXT("UnshelveShelf", "Unshelving..."); }

	int32 ShelfId = 0;
};

#undef LOCTEXT_NAMESPACE

namespace LodeWorkers
{
	/** Registers every worker with the provider. */
	void RegisterAll(FLodeProvider& Provider);

	/** Smallest lode version the plugin works with (it needs status --remote, cat -o and --paths-from). */
	inline constexpr int32 MinimumVersion[3] = { 0, 2, 0 };
	/** Server API version the plugin speaks (`lode --version` "api"). */
	inline constexpr int32 SupportedApi = 1;

	/** True when Version ("0.2.0", "0.10.1-rc1") is at least the minimum. Public for tests. */
	bool IsVersionSupported(const FString& Version);

	/** Hands a new worker what it needs from the provider (game thread, before the command is queued). */
	void PrepareWorker(ILodeWorker& Worker, const FLodeProvider& Provider, const FLodeChangelist& Changelist);

	/** A changelist name made from a description: letters, digits, '-' and '_', unique among Existing. */
	FString MakeChangelistName(const FString& Description, const TArray<FString>& Existing);
}
