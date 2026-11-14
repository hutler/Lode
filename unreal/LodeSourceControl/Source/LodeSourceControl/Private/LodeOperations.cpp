// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeOperations.h"

#include "HAL/FileManager.h"
#include "HAL/PlatformProcess.h"
#include "Misc/FileHelper.h"
#include "Misc/PackageName.h"
#include "Misc/Paths.h"
#include "Misc/ScopeLock.h"
#include "PackageTools.h"
#include "SourceControlOperations.h"
#include "UObject/Package.h"
#include "UObject/UObjectGlobals.h"

#include "LodeJsonFields.h"
#include "LodeLog.h"
#include "LodeParsers.h"
#include "LodePaths.h"
#include "LodeProvider.h"
#include "LodeSourceControlModule.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl"

// ---------------------------------------------------------------------------------------------
// Helpers shared by the workers. All run on pool threads.
// ---------------------------------------------------------------------------------------------

namespace
{
	/** The command's files that lie inside the workspace (lode refuses the whole call otherwise). */
	TArray<FString> InWorkspace(const FLodeCommand& Command, const TArray<FString>& Files)
	{
		TArray<FString> Out;
		for (const FString& File : Files)
		{
			if (LodePaths::IsInWorkspace(Command.Workspace.Root, File))
			{
				Out.Add(File);
			}
		}
		return Out;
	}

	/** Records for files outside the workspace: not controlled, and nothing to ask lode. */
	void AddOutsideRecords(const FLodeCommand& Command, const TArray<FString>& Files, TArray<FLodeFileRecord>& OutRecords)
	{
		for (const FString& File : Files)
		{
			if (!LodePaths::IsInWorkspace(Command.Workspace.Root, File) && File != Command.Workspace.Root)
			{
				FLodeFileRecord Record;
				Record.Filename = File;
				Record.Status = ELodeFileStatus::Untracked;
				OutRecords.Add(MoveTemp(Record));
			}
		}
	}

	/** Runs `lode status --unchanged [--remote]` for files and appends their records. */
	bool RunStatus(FLodeCommand& Command, const TArray<FString>& Files, bool bRemote, TArray<FLodeFileRecord>& OutRecords,
		FString* OutBranch = nullptr, int32* OutRev = nullptr)
	{
		AddOutsideRecords(Command, Files, OutRecords);
		const TArray<FString> Inside = InWorkspace(Command, Files);
		if (Inside.Num() == 0 && Files.Num() > 0)
		{
			return true;
		}

		FLodeInvocation Invocation{ TEXT("status") };
		if (Inside.Num() > 0)
		{
			Invocation.Args.Add(TEXT("--unchanged"));
		}
		if (bRemote)
		{
			Invocation.Args.Add(TEXT("--remote"));
		}
		Invocation.Paths = Inside;
		Invocation.bPathsFromFile = true;
		const FLodeResult Result = Command.Run(Invocation);
		if (!Result.IsOk())
		{
			return false;
		}
		FString Branch;
		int32 Rev = 0;
		LodeParsers::ParseStatus(*Result.Json, Command.Workspace.Root, Inside, OutRecords, Branch, Rev);
		if (OutBranch) { *OutBranch = Branch; }
		if (OutRev) { *OutRev = Rev; }
		return true;
	}

	/**
	 * Merges status requests that arrive while one is running into the next call (spec section
	 * 6). The first waiting request runs the merged call; the others wait for it.
	 */
	class FStatusBatcher
	{
	public:
		struct FBatch
		{
			TArray<FString> Files;
			TSet<FString> Seen;
			std::atomic<bool> bDone { false };
			bool bOk = false;
			TArray<FLodeFileRecord> Records;
			TArray<FText> Errors;
		};
		using FBatchRef = TSharedRef<FBatch, ESPMode::ThreadSafe>;

		static FStatusBatcher& Get()
		{
			static FStatusBatcher Instance;
			return Instance;
		}

		FBatchRef Run(FLodeCommand& Command, const TArray<FString>& Files)
		{
			TSharedPtr<FBatch, ESPMode::ThreadSafe> Mine;
			{
				FScopeLock ScopeLock(&Lock);
				if (!Pending.IsValid())
				{
					Pending = MakeShared<FBatch, ESPMode::ThreadSafe>();
				}
				for (const FString& File : Files)
				{
					if (!Pending->Seen.Contains(File))
					{
						Pending->Seen.Add(File);
						Pending->Files.Add(File);
					}
				}
				Mine = Pending;
			}

			while (!Mine->bDone)
			{
				bool bDrive = false;
				{
					FScopeLock ScopeLock(&Lock);
					if (!bRunning && Pending == Mine)
					{
						bRunning = true;
						Pending.Reset();
						bDrive = true;
					}
				}
				if (bDrive)
				{
					const int32 ErrorsBefore = Command.ErrorMessages.Num();
					Mine->bOk = RunStatus(Command, Mine->Files, /*bRemote*/ false, Mine->Records);
					for (int32 Index = ErrorsBefore; Index < Command.ErrorMessages.Num(); ++Index)
					{
						Mine->Errors.Add(Command.ErrorMessages[Index]);
					}
					Command.ErrorMessages.SetNum(ErrorsBefore);
					Mine->bDone = true;
					FScopeLock ScopeLock(&Lock);
					bRunning = false;
				}
				else
				{
					FPlatformProcess::Sleep(0.005f);
				}
			}
			return Mine.ToSharedRef();
		}

	private:
		FCriticalSection Lock;
		TSharedPtr<FBatch, ESPMode::ThreadSafe> Pending;
		bool bRunning = false;
	};

	/** The files of a command, or those of its changelist when it named none. */
	TArray<FString> FilesOrChangelist(const FLodeCommand& Command)
	{
		if (Command.Files.Num() > 0 || !Command.Changelist.IsInitialized())
		{
			return Command.Files;
		}
		TArray<FString> Files;
		for (const TPair<FString, FLodeFileRecord>& Item : Command.KnownRecords)
		{
			const FString Changelist = Item.Value.Changelist.IsEmpty() ? FLodeChangelist::DefaultName : Item.Value.Changelist;
			if (Changelist == Command.Changelist.GetName())
			{
				Files.Add(Item.Key);
			}
		}
		return Files;
	}

	/** Writes text to a temporary UTF-8 file for options like `commit -F`. */
	FString WriteTempText(const FLodeCommand& Command, const FString& Text)
	{
		IFileManager::Get().MakeDirectory(*Command.RunnerConfig.TempDirectory, true);
		const FString Path = FPaths::ConvertRelativePathToFull(FPaths::CreateTempFilename(*Command.RunnerConfig.TempDirectory, TEXT("lode-message-"), TEXT(".txt")));
		return FFileHelper::SaveStringToFile(Text, *Path, FFileHelper::EEncodingOptions::ForceUTF8WithoutBOM) ? Path : FString();
	}

	FString JoinDepotPaths(const FLodeCommand& Command, const TArray<FString>& Files, int32 Max = 10)
	{
		TArray<FString> Names;
		for (int32 Index = 0; Index < Files.Num() && Index < Max; ++Index)
		{
			const FString Depot = LodePaths::ToDepotPath(Command.Workspace.Root, Files[Index]);
			Names.Add(Depot.IsEmpty() ? Files[Index] : Depot);
		}
		if (Files.Num() > Max)
		{
			Names.Add(FString::Printf(TEXT("and %d more"), Files.Num() - Max));
		}
		return FString::Join(Names, TEXT(", "));
	}

	/** Reloads assets the editor has loaded whose files lode just replaced, unless they have unsaved edits. */
	void ReloadChangedPackages(const TArray<FString>& Files)
	{
		TArray<UPackage*> ToReload;
		for (const FString& File : Files)
		{
			FString PackageName;
			if (FPackageName::TryConvertFilenameToLongPackageName(File, PackageName))
			{
				UPackage* Package = FindPackage(nullptr, *PackageName);
				if (Package && !Package->IsDirty())
				{
					ToReload.Add(Package);
				}
			}
		}
		if (ToReload.Num() > 0)
		{
			UPackageTools::ReloadPackages(ToReload);
		}
	}

	/** Base for workers whose result is a set of file records. */
	class FRecordsWorker : public ILodeWorker
	{
	public:
		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			return LodeProvider.ApplyRecords(Records);
		}

	protected:
		TArray<FLodeFileRecord> Records;
	};
}

bool LodeWorkers::IsVersionSupported(const FString& Version)
{
	FString Core = Version;
	int32 Dash = INDEX_NONE;
	if (Core.FindChar(TEXT('-'), Dash))
	{
		Core.LeftInline(Dash);
	}
	TArray<FString> Parts;
	Core.ParseIntoArray(Parts, TEXT("."), true);
	if (Parts.Num() < 2)
	{
		return false;
	}
	for (int32 Index = 0; Index < 3; ++Index)
	{
		const int32 Have = Parts.IsValidIndex(Index) ? FCString::Atoi(*Parts[Index]) : 0;
		if (Have != MinimumVersion[Index])
		{
			return Have > MinimumVersion[Index];
		}
	}
	return true;
}

FString LodeWorkers::MakeChangelistName(const FString& Description, const TArray<FString>& Existing)
{
	FString FirstLine = Description;
	int32 NewLine = INDEX_NONE;
	if (FirstLine.FindChar(TEXT('\n'), NewLine))
	{
		FirstLine.LeftInline(NewLine);
	}
	FString Name;
	bool bDash = false;
	for (const TCHAR Char : FirstLine.TrimStartAndEnd())
	{
		if (FChar::IsAlnum(Char) && Char < 128)
		{
			Name.AppendChar(Char);
			bDash = false;
		}
		else if (!bDash && Name.Len() > 0)
		{
			Name.AppendChar(TEXT('-'));
			bDash = true;
		}
		if (Name.Len() >= 32)
		{
			break;
		}
	}
	while (Name.EndsWith(TEXT("-")))
	{
		Name.LeftChopInline(1);
	}
	if (Name.IsEmpty() || Name.Equals(FLodeChangelist::DefaultName, ESearchCase::IgnoreCase))
	{
		Name = TEXT("changes");
	}
	FString Candidate = Name;
	for (int32 Suffix = 2; Existing.Contains(Candidate); ++Suffix)
	{
		Candidate = FString::Printf(TEXT("%s-%d"), *Name, Suffix);
	}
	return Candidate;
}

// ---------------------------------------------------------------------------------------------
// Connect (unreal T03)
// ---------------------------------------------------------------------------------------------

namespace
{
	class FConnectWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "Connect"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const bool bKnownRoot = !Command.Workspace.Root.IsEmpty() && FPaths::DirectoryExists(Command.Workspace.Root / TEXT(".lode"));
			Info.Root = bKnownRoot ? Command.Workspace.Root : LodePaths::FindWorkspaceRoot(FPaths::ProjectDir());
			bWorkspaceFound = !Info.Root.IsEmpty();
			bLodeFound = !Command.RunnerConfig.Executable.IsEmpty() && FPaths::FileExists(Command.RunnerConfig.Executable);
			if (!bLodeFound)
			{
				Problem = TEXT("The lode program was not found. Install Lode (it puts `lode` on PATH), or set the path to lode in these settings.");
				Command.ErrorMessages.Add(FText::FromString(Problem));
				return false;
			}

			FLodeResult Version = Command.Run({ TEXT("--version") });
			if (!Version.IsOk())
			{
				Problem = Version.Describe();
				return false;
			}
			Version.Json->TryGetStringField(LodeJson::Version, Info.ClientVersion);
			Version.Json->TryGetStringField(LodeJson::Api, Info.ClientApi);
			if (!LodeWorkers::IsVersionSupported(Info.ClientVersion) || FCString::Atoi(*Info.ClientApi) < LodeWorkers::SupportedApi)
			{
				Problem = FString::Printf(TEXT("lode %s is too old for this plugin. Install lode %d.%d.%d or later."),
					*Info.ClientVersion, LodeWorkers::MinimumVersion[0], LodeWorkers::MinimumVersion[1], LodeWorkers::MinimumVersion[2]);
				Command.ErrorMessages.Add(FText::FromString(Problem));
				return false;
			}

			if (!bWorkspaceFound)
			{
				Problem = FString::Printf(TEXT("This project is not inside a Lode workspace. Check out the depot (`lode checkout <server>/<depot> <folder>`) so that the project folder %s is inside it, or start one here with `lode init <server>/<depot> --template unreal`."),
					*FPaths::ConvertRelativePathToFull(FPaths::ProjectDir()));
				Command.ErrorMessages.Add(FText::FromString(Problem));
				return false;
			}
			Command.Workspace.Root = Info.Root;
			Command.RunnerConfig.WorkingDirectory = Info.Root;

			// Local: branch, revision and the changed files. No request to the server.
			FString Branch;
			int32 Rev = 0;
			if (!RunStatus(Command, TArray<FString>(), false, Records, &Branch, &Rev))
			{
				Problem = Command.ErrorMessages.Num() > 0 ? Command.ErrorMessages.Last().ToString() : TEXT("lode status failed.");
				return false;
			}
			Info.Branch = Branch;
			Info.Rev = Rev;

			// Server and user.
			const FLodeResult WhoAmI = Command.Run({ TEXT("whoami") }, /*bReportFailure*/ false);
			if (WhoAmI.IsOk())
			{
				WhoAmI.Json->TryGetStringField(LodeJson::Server, Info.Server);
				WhoAmI.Json->TryGetStringField(LodeJson::Name, Info.UserName);
				WhoAmI.Json->TryGetStringField(LodeJson::Kind, Info.UserKind);
				WhoAmI.Json->TryGetStringArrayField(LodeJson::Scopes, Info.Scopes);
				bSignedIn = true;
			}
			else if (WhoAmI.Outcome == ELodeOutcome::Auth)
			{
				Problem = TEXT("You are not signed in to the Lode server. Sign in below with a token or your password, or run `lode login` in a terminal.");
				Command.ErrorMessages.Add(FText::FromString(Problem));
				return false;
			}
			else
			{
				Problem = WhoAmI.Describe();
				Command.AddFailure(WhoAmI);
				return false;
			}

			// Depot name: lode status does not print it (proposed in unreal/docs/SPEC.md section 5).
			const FLodeResult Log = Command.Run({ TEXT("log"), TEXT("-n"), TEXT("1") }, false);
			if (Log.IsOk())
			{
				Log.Json->TryGetStringField(LodeJson::Depot, Info.Depot);
			}

			for (const FString& Scope : { FString(TEXT("lock")), FString(TEXT("write")) })
			{
				if (!Info.Scopes.Contains(Scope))
				{
					Command.InfoMessages.Add(FText::Format(LOCTEXT("MissingScope", "Your Lode token has no '{0}' scope, so some actions will be refused. Sign in again with a token that has it."), FText::FromString(Scope)));
				}
			}
			Command.InfoMessages.Add(FText::Format(LOCTEXT("Connected", "Connected to Lode: {0} on {1}, depot {2}, branch {3} at r{4}."),
				FText::FromString(Info.UserName), FText::FromString(Info.Server), FText::FromString(Info.Depot), FText::FromString(Info.Branch), FText::AsNumber(Info.Rev)));
			Problem.Reset();
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			LodeProvider.ApplyWorkspace(Info, bLodeFound, bWorkspaceFound, bSignedIn, Problem);
			return LodeProvider.ApplyRecords(Records);
		}

	private:
		FLodeWorkspaceInfo Info;
		bool bLodeFound = false;
		bool bWorkspaceFound = false;
		bool bSignedIn = false;
		FString Problem;
		TArray<FLodeFileRecord> Records;
	};

	class FLoginWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "LodeLogin"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FLodeLogin, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FLodeLogin>(Command.Operation);
			FLodeInvocation Invocation{ TEXT("login") };
			switch (Operation->Method)
			{
			case FLodeLogin::EMethod::Token:
				Invocation.Args.Add(TEXT("--token-stdin"));
				Invocation.StdIn = Operation->Secret;
				break;
			case FLodeLogin::EMethod::Password:
				Invocation.Args.Append({ TEXT("--name"), Operation->UserName, TEXT("--password-stdin") });
				Invocation.StdIn = Operation->Secret;
				break;
			case FLodeLogin::EMethod::SingleSignOn:
				// Opens the browser; lode waits up to five minutes for it.
				Invocation.Args.Add(TEXT("--sso"));
				Invocation.TimeoutSeconds = 330.0;
				break;
			}
			// The secret is not needed past this point.
			Operation->Secret.Reset();

			const FLodeResult Result = Command.Run(Invocation);
			Invocation.StdIn.Reset();
			if (!Result.IsOk())
			{
				return false;
			}
			Result.Json->TryGetStringField(LodeJson::Name, SignedInAs);
			Operation->SignedInAs = SignedInAs;
			Command.InfoMessages.Add(FText::Format(LOCTEXT("SignedIn", "Signed in to Lode as {0}."), FText::FromString(SignedInAs)));
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override { return false; }

	private:
		FString SignedInAs;
	};
}

// ---------------------------------------------------------------------------------------------
// Status and server information (unreal T04)
// ---------------------------------------------------------------------------------------------

namespace
{
	class FUpdateStatusWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "UpdateStatus"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FUpdateStatus, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FUpdateStatus>(Command.Operation);

			bool bOk = true;
			if (Command.Files.Num() == 0 || Operation->ShouldGetOpenedOnly())
			{
				// The whole workspace's pending changes (`lode status` without paths lists only those).
				bOk = RunStatus(Command, TArray<FString>(), false, Records);
			}
			else
			{
				const FStatusBatcher::FBatchRef Batch = FStatusBatcher::Get().Run(Command, Command.Files);
				bOk = Batch->bOk;
				Records = Batch->Records;
				Command.ErrorMessages.Append(Batch->Errors);
			}

			if (bOk && Operation->ShouldUpdateHistory())
			{
				for (const FString& File : InWorkspace(Command, Command.Files))
				{
					const FString DepotPath = LodePaths::ToDepotPath(Command.Workspace.Root, File);
					const FLodeResult Log = Command.Run({ TEXT("log"), DepotPath, TEXT("-v"), TEXT("--follow"), TEXT("-n"), TEXT("200") }, false);
					if (Log.IsOk())
					{
						History.Add(File, LodeParsers::ParseFileLog(*Log.Json, DepotPath));
					}
					else if (Log.Error.Code != LodeErrorCode::NotFound)
					{
						Command.AddFailure(Log);
					}
				}
			}
			return bOk;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			bool bChanged = FRecordsWorker::UpdateStates(LodeProvider);
			for (const TPair<FString, TArray<FLodeRevisionRecord>>& Item : History)
			{
				TArray<FLodeRevisionRef> Revisions;
				for (const FLodeRevisionRecord& Record : Item.Value)
				{
					Revisions.Add(MakeShared<FLodeRevision, ESPMode::ThreadSafe>(Item.Key, Record));
				}
				LodeProvider.GetStateInternal(Item.Key)->SetHistory(MoveTemp(Revisions));
				bChanged = true;
			}
			return bChanged;
		}

	private:
		TMap<FString, TArray<FLodeRevisionRecord>> History;
	};

	class FRefreshServerWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "LodeRefreshServer"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			return RunStatus(Command, Command.Files, /*bRemote*/ true, Records);
		}
	};
}

// ---------------------------------------------------------------------------------------------
// Check out (lock), add, delete, revert, sync, submit (unreal T05, T06)
// ---------------------------------------------------------------------------------------------

namespace
{
	class FCheckOutWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "CheckOut"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TArray<FString> ToLock;
			for (const FString& File : InWorkspace(Command, Command.Files))
			{
				if (!Command.KnownRecord(File).bLockedHere)
				{
					ToLock.Add(File);
				}
			}
			bool bOk = true;
			if (ToLock.Num() > 0)
			{
				FLodeInvocation Lock{ TEXT("lock") };
				Lock.Paths = ToLock;
				Lock.bPathsFromFile = true;
				bOk = Command.Run(Lock).IsOk();
			}
			// Refresh with server information either way: a refusal names who holds the lock.
			RunStatus(Command, Command.Files, /*bRemote*/ true, Records);
			return bOk;
		}
	};

	class FMarkForAddWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "MarkForAdd"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const TArray<FString> Files = InWorkspace(Command, Command.Files);
			if (Files.Num() < Command.Files.Num())
			{
				Command.ErrorMessages.Add(LOCTEXT("AddOutside", "Some files are outside the Lode workspace and cannot be added."));
			}
			bool bOk = Files.Num() > 0;
			if (bOk)
			{
				FLodeInvocation Add{ TEXT("add") };
				Add.Paths = Files;
				Add.bPathsFromFile = true;
				bOk = Command.Run(Add).IsOk();
			}
			RunStatus(Command, Command.Files, false, Records);
			return bOk;
		}
	};

	class FDeleteWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "Delete"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const TArray<FString> Files = InWorkspace(Command, Command.Files);
			// Deleting a file that needs a lock needs the lock too, or the submit is refused.
			TArray<FString> ToLock;
			for (const FString& File : Files)
			{
				const FLodeFileRecord Record = Command.KnownRecord(File);
				if (Record.bLockRequired && !Record.bLockedHere && Record.Status != ELodeFileStatus::Added)
				{
					ToLock.Add(File);
				}
			}
			bool bOk = Files.Num() > 0;
			if (bOk && ToLock.Num() > 0)
			{
				FLodeInvocation Lock{ TEXT("lock") };
				Lock.Paths = ToLock;
				Lock.bPathsFromFile = true;
				bOk = Command.Run(Lock).IsOk();
			}
			if (bOk)
			{
				FLodeInvocation Remove{ TEXT("rm"), TEXT("--force") };
				Remove.Paths = Files;
				Remove.bPathsFromFile = true;
				bOk = Command.Run(Remove).IsOk();
			}
			RunStatus(Command, Command.Files, ToLock.Num() > 0, Records);
			return bOk;
		}
	};

	class FRevertWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "Revert"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FRevert, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FRevert>(Command.Operation);
			const TArray<FString> Files = InWorkspace(Command, FilesOrChangelist(Command));

			TArray<FString> ToRevert;
			TArray<FString> ToUnlock;
			TArray<FString> Added;
			for (const FString& File : Files)
			{
				const FLodeFileRecord Record = Command.KnownRecord(File);
				const bool bChanged = Record.Status != ELodeFileStatus::Unchanged && Record.Status != ELodeFileStatus::Untracked && Record.Status != ELodeFileStatus::Ignored;
				if (bChanged || Record.Status == ELodeFileStatus::Unknown)
				{
					ToRevert.Add(File);
				}
				if (Record.bLockedHere || Record.Status == ELodeFileStatus::Unknown)
				{
					ToUnlock.Add(File);
				}
				if (Record.Status == ELodeFileStatus::Added)
				{
					Added.Add(File);
				}
			}

			bool bOk = true;
			// A soft revert keeps the local changes and only gives the files back (releases locks).
			if (!Operation->IsSoftRevert() && ToRevert.Num() > 0)
			{
				FLodeInvocation Revert{ TEXT("revert") };
				Revert.Paths = ToRevert;
				Revert.bPathsFromFile = true;
				bOk = Command.Run(Revert).IsOk();
			}
			// `lode revert` keeps locks; the editor's revert gives them back.
			if (bOk && ToUnlock.Num() > 0)
			{
				FLodeInvocation Unlock{ TEXT("unlock") };
				Unlock.Paths = ToUnlock;
				Unlock.bPathsFromFile = true;
				const FLodeResult Result = Command.Run(Unlock, false);
				// A file that was not locked is not an error here.
				if (!Result.IsOk() && Result.Error.Code != LodeErrorCode::NotFound && Result.Error.Code != LodeErrorCode::ModuleDisabled)
				{
					Command.AddFailure(Result);
					bOk = false;
				}
			}
			if (bOk && !Operation->IsSoftRevert() && Operation->ShouldDeleteNewFiles())
			{
				for (const FString& File : Added)
				{
					IFileManager::Get().Delete(*File, false, true, true);
				}
			}
			RunStatus(Command, Files, ToUnlock.Num() > 0, Records);
			return bOk;
		}
	};

	class FRevertUnchangedWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "RevertUnchanged"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			// Fresh state first: "unchanged" must be true now, not at the last refresh.
			TArray<FLodeFileRecord> Current;
			if (!RunStatus(Command, InWorkspace(Command, FilesOrChangelist(Command)), false, Current))
			{
				return false;
			}
			TArray<FString> ToUnlock;
			for (const FLodeFileRecord& Record : Current)
			{
				if (Record.bLockedHere && Record.Status == ELodeFileStatus::Unchanged)
				{
					ToUnlock.Add(Record.Filename);
				}
			}
			bool bOk = true;
			if (ToUnlock.Num() > 0)
			{
				FLodeInvocation Unlock{ TEXT("unlock") };
				Unlock.Paths = ToUnlock;
				Unlock.bPathsFromFile = true;
				bOk = Command.Run(Unlock).IsOk();
				RunStatus(Command, ToUnlock, true, Records);
			}
			else
			{
				Records = MoveTemp(Current);
			}
			return bOk;
		}
	};

	class FSyncWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "Sync"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FSync, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FSync>(Command.Operation);
			// Lode brings the whole workspace to one revision; it has no per-file sync.
			FLodeInvocation Update{ TEXT("update") };
			const FString Revision = Operation->GetRevision().TrimStartAndEnd();
			const FString Digits = Revision.StartsWith(TEXT("r")) ? Revision.RightChop(1) : Revision;
			if (!Digits.IsEmpty() && Digits.IsNumeric())
			{
				Update.Args.Append({ TEXT("-r"), Digits });
			}
			Update.TimeoutSeconds = 3600.0;
			const FLodeResult Result = Command.Run(Update);
			if (!Result.IsOk())
			{
				return false;
			}

			int32 Rev = 0;
			Result.Json->TryGetNumberField(LodeJson::Rev, Rev);
			const TArray<TSharedPtr<FJsonValue>>* Changed = nullptr;
			if (Result.Json->TryGetArrayField(LodeJson::Files, Changed) && Changed)
			{
				for (const TSharedPtr<FJsonValue>& Value : *Changed)
				{
					const TSharedPtr<FJsonObject>* File = nullptr;
					FString Path;
					if (Value.IsValid() && Value->TryGetObject(File) && File && (*File)->TryGetStringField(LodeJson::Path, Path))
					{
						UpdatedFiles.Add(LodePaths::ToFilename(Command.Workspace.Root, Path));
					}
				}
			}
			const TArray<FString> Conflicts = LodeParsers::ParseConflicts(*Result.Json, Command.Workspace.Root);
			Command.InfoMessages.Add(FText::Format(LOCTEXT("Synced", "Lode workspace is at r{0}; {1} file(s) changed."), FText::AsNumber(Rev), FText::AsNumber(UpdatedFiles.Num())));
			if (Conflicts.Num() > 0)
			{
				Command.ErrorMessages.Add(FText::Format(LOCTEXT("SyncConflicts", "{0} file(s) changed both here and on the server and are now in conflict: {1}. Binary assets cannot be merged: resolve each to your version or theirs (right-click the asset, Lode)."),
					FText::AsNumber(Conflicts.Num()), FText::FromString(JoinDepotPaths(Command, Conflicts))));
			}

			TArray<FString> ToRefresh = Command.Files;
			ToRefresh.Append(UpdatedFiles);
			RunStatus(Command, ToRefresh, true, Records);
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			// Reload assets the update changed that the editor has loaded and the person has not
			// edited; the editor only reloads the ones it asked to sync.
			ReloadChangedPackages(UpdatedFiles);
			return FRecordsWorker::UpdateStates(LodeProvider);
		}

	private:
		TArray<FString> UpdatedFiles;
	};

	class FCheckInWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "CheckIn"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FCheckIn, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FCheckIn>(Command.Operation);
			const TArray<FString> Files = InWorkspace(Command, FilesOrChangelist(Command));
			if (Files.Num() == 0)
			{
				Command.ErrorMessages.Add(LOCTEXT("NothingToSubmit", "There is nothing to submit."));
				return false;
			}

			// Files the editor submits without having added them.
			TArray<FString> ToAdd;
			for (const FString& File : Files)
			{
				const FLodeFileRecord Record = Command.KnownRecord(File);
				if (Record.Status == ELodeFileStatus::Untracked && FPaths::FileExists(File))
				{
					ToAdd.Add(File);
				}
			}
			if (ToAdd.Num() > 0)
			{
				FLodeInvocation Add{ TEXT("add") };
				Add.Paths = ToAdd;
				Add.bPathsFromFile = true;
				if (!Command.Run(Add).IsOk())
				{
					return false;
				}
			}

			FString Description = Operation->GetDescription().ToString().TrimStartAndEnd();
			if (Description.IsEmpty())
			{
				Description = TEXT("Submitted from the Unreal Editor");
			}
			const FString MessageFile = WriteTempText(Command, Description);
			FLodeInvocation Commit{ TEXT("commit"), TEXT("-F"), MessageFile };
			if (Operation->GetKeepCheckedOut() || Command.bKeepLocksDefault)
			{
				Commit.Args.Add(TEXT("--keep-locks"));
			}
			Commit.Paths = Files;
			Commit.bPathsFromFile = true;
			const FLodeResult Result = Command.Run(Commit);
			IFileManager::Get().Delete(*MessageFile, false, true, true);

			if (Result.IsOk())
			{
				int32 Rev = 0;
				Result.Json->TryGetNumberField(LodeJson::Rev, Rev);
				const FText Message = FText::Format(LOCTEXT("Submitted", "Submitted r{0}: {1} file(s)."), FText::AsNumber(Rev), FText::AsNumber(Files.Num()));
				Operation->SetSuccessMessage(Message);
				Command.InfoMessages.Add(Message);

				// An emptied changelist goes away, as it does with other providers.
				if (Command.Changelist.IsInitialized() && !Command.Changelist.IsDefault())
				{
					Command.Run({ TEXT("changelist"), TEXT("delete"), Command.Changelist.GetName() }, false);
					bChangelistDeleted = true;
				}
			}
			RunStatus(Command, Files, true, Records);
			Changelist = Command.Changelist;
			return Result.IsOk();
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			if (bChangelistDeleted)
			{
				LodeProvider.RemoveChangelistFromCache(Changelist);
			}
			return FRecordsWorker::UpdateStates(LodeProvider);
		}

	private:
		bool bChangelistDeleted = false;
		FLodeChangelist Changelist;
	};
}

// ---------------------------------------------------------------------------------------------
// Rename, copy and resolve (unreal T08)
// ---------------------------------------------------------------------------------------------

namespace
{
	class FCopyWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "Copy"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FCopy, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FCopy>(Command.Operation);
			if (Command.Files.Num() != 1)
			{
				Command.ErrorMessages.Add(LOCTEXT("CopyOne", "Copy and move work on one file at a time."));
				return false;
			}
			const FString Source = Command.Files[0];
			const FString Destination = LodePaths::Normalize(Operation->GetDestination());
			const bool bMove = Operation->CopyMethod == FCopy::ECopyMethod::Branch;

			// The editor writes the new file itself and, for a rename, leaves a redirector at the old
			// path. A copy that already happened on disk is recorded with --after; when the old file
			// is still there (the redirector), it is recorded as a copy so the old path stays tracked
			// and the new one keeps the history.
			const bool bSourceOnDisk = FPaths::FileExists(Source);
			const bool bDestinationOnDisk = FPaths::FileExists(Destination);
			const bool bUseMove = bMove && !bSourceOnDisk;
			FLodeInvocation Invocation{ bUseMove ? TEXT("mv") : TEXT("cp") };
			if (bDestinationOnDisk)
			{
				Invocation.Args.Add(TEXT("--after"));
			}
			Invocation.Args.Append({ TEXT("--"), LodePaths::ToDepotPath(Command.Workspace.Root, Source), LodePaths::ToDepotPath(Command.Workspace.Root, Destination) });
			const bool bOk = Command.Run(Invocation).IsOk();
			RunStatus(Command, { Source, Destination }, false, Records);
			return bOk;
		}
	};

	class FResolveWorker : public FRecordsWorker
	{
	public:
		explicit FResolveWorker(FName InName) : Name(InName) {}

		virtual FName GetName() const override { return Name; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			// The editor's own Resolve comes after its merge tool wrote the result into the local
			// file, so it keeps the local file. The Lode menu can take either side.
			if (Name == TEXT("LodeResolve"))
			{
				bTheirs = StaticCastSharedRef<FLodeResolve>(Command.Operation)->bTakeTheirs;
			}
			FLodeInvocation Resolve{ TEXT("resolve"), bTheirs ? TEXT("--theirs") : TEXT("--mine") };
			Resolve.Paths = InWorkspace(Command, Command.Files);
			const bool bOk = Command.RunAll(Resolve);
			Files = Command.Files;
			RunStatus(Command, Command.Files, false, Records);
			return bOk;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			if (bTheirs)
			{
				ReloadChangedPackages(Files);
			}
			return FRecordsWorker::UpdateStates(LodeProvider);
		}

	private:
		FName Name;
		bool bTheirs = false;
		TArray<FString> Files;
	};
}

// ---------------------------------------------------------------------------------------------
// Changelists and shelves (unreal T09)
// ---------------------------------------------------------------------------------------------

namespace
{
	/** Reads every changelist with its files, and the shelved files of the ones that have a shelf. */
	class FUpdateChangelistsWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "UpdateChangelistsStatus"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const FLodeResult List = Command.Run({ TEXT("changelist"), TEXT("list") });
			if (!List.IsOk())
			{
				return false;
			}
			Changelists = LodeParsers::ParseChangelists(*List.Json, Command.Workspace.Root);
			if (!RunStatus(Command, TArray<FString>(), false, Records))
			{
				return false;
			}

			// Shelves remembered for these changelists, read on the game thread when the command was made.
			for (const TPair<FString, int32>& Item : ShelfIds)
			{
				const FLodeResult Show = Command.Run({ TEXT("shelf"), TEXT("show"), FString::FromInt(Item.Value) }, false);
				if (Show.IsOk())
				{
					Shelves.Add(Item.Key, LodeParsers::ParseShelf(*Show.Json, Command.Workspace.Root));
				}
				else if (Show.Error.Code == LodeErrorCode::NotFound)
				{
					Shelves.Add(Item.Key, FLodeShelfRecord()); // gone: forget it
				}
			}
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			LodeProvider.ApplyRecords(Records);

			TSet<FLodeChangelist> Seen;
			for (const FLodeChangelistRecord& Record : Changelists)
			{
				const FLodeChangelist Changelist(Record.Name);
				Seen.Add(Changelist);
				FLodeChangelistStateRef State = LodeProvider.GetChangelistStateInternal(Changelist);
				State->Description = Record.Description;
				State->Files.Reset();
				for (const FString& File : Record.Filenames)
				{
					State->Files.Add(LodeProvider.GetStateInternal(File));
				}
				State->TimeStamp = FDateTime::Now();

				State->ShelvedFiles.Reset();
				State->ShelfId = 0;
				if (const FLodeShelfRecord* Shelf = Shelves.Find(Record.Name))
				{
					if (Shelf->Id > 0 && Shelf->State == TEXT("open"))
					{
						State->ShelfId = Shelf->Id;
						State->ShelfVersion = Shelf->Version;
						for (int32 Index = 0; Index < Shelf->Filenames.Num(); ++Index)
						{
							FLodeFileRecord ShelvedRecord;
							ShelvedRecord.Filename = Shelf->Filenames[Index];
							const FString& Action = Shelf->Actions[Index];
							ShelvedRecord.Status = Action == TEXT("add") ? ELodeFileStatus::Added : (Action == TEXT("delete") ? ELodeFileStatus::Deleted : ELodeFileStatus::Modified);
							TSharedRef<FLodeState, ESPMode::ThreadSafe> ShelvedState = MakeShared<FLodeState, ESPMode::ThreadSafe>(ShelvedRecord.Filename);
							ShelvedState->SetRecord(ShelvedRecord);
							State->ShelvedFiles.Add(ShelvedState);
						}
					}
					else
					{
						LodeProvider.SetShelfForChangelist(Record.Name, 0, 0);
					}
				}
			}

			// Changelists that no longer exist in Lode.
			TArray<FLodeChangelist> Gone;
			for (const FSourceControlChangelistRef& Changelist : LodeProvider.GetChangelists(EStateCacheUsage::Use))
			{
				const FLodeChangelist& LodeChangelist = static_cast<const FLodeChangelist&>(Changelist.Get());
				if (!Seen.Contains(LodeChangelist))
				{
					Gone.Add(LodeChangelist);
				}
			}
			for (const FLodeChangelist& Changelist : Gone)
			{
				LodeProvider.RemoveChangelistFromCache(Changelist);
			}
			return true;
		}

		/** Filled on the game thread before the command runs. */
		TMap<FString, int32> ShelfIds;

	private:
		TArray<FLodeChangelistRecord> Changelists;
		TArray<FLodeFileRecord> Records;
		TMap<FString, FLodeShelfRecord> Shelves;
	};

	class FNewChangelistWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "NewChangelist"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FNewChangelist, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FNewChangelist>(Command.Operation);
			const FLodeResult List = Command.Run({ TEXT("changelist"), TEXT("list") });
			if (!List.IsOk())
			{
				return false;
			}
			TArray<FString> Existing;
			for (const FLodeChangelistRecord& Record : LodeParsers::ParseChangelists(*List.Json, Command.Workspace.Root))
			{
				Existing.Add(Record.Name);
			}
			Description = Operation->GetDescription().ToString().TrimStartAndEnd();
			Name = LodeWorkers::MakeChangelistName(Description, Existing);
			const FLodeResult Created = Command.Run({ TEXT("changelist"), TEXT("new"), Name, TEXT("-m"), Description });
			if (!Created.IsOk())
			{
				return false;
			}
			Operation->SetNewChangelist(MakeShared<FLodeChangelist, ESPMode::ThreadSafe>(Name));

			const TArray<FString> Files = InWorkspace(Command, Command.Files);
			if (Files.Num() > 0)
			{
				FLodeInvocation Move{ TEXT("changelist"), TEXT("move"), Name };
				Move.Paths = Files;
				Move.bPathsFromFile = true;
				if (!Command.Run(Move).IsOk())
				{
					return false;
				}
				RunStatus(Command, Files, false, Records);
			}
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			FLodeChangelistStateRef State = LodeProvider.GetChangelistStateInternal(FLodeChangelist(Name));
			State->Description = Description;
			State->TimeStamp = FDateTime::Now();
			FRecordsWorker::UpdateStates(LodeProvider);
			return true;
		}

	private:
		FString Name;
		FString Description;
	};

	class FDeleteChangelistWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "DeleteChangelist"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			Changelist = Command.Changelist;
			if (Changelist.IsDefault())
			{
				Command.ErrorMessages.Add(LOCTEXT("DeleteDefault", "The default changelist cannot be deleted."));
				return false;
			}
			bDeleted = Command.Run({ TEXT("changelist"), TEXT("delete"), Changelist.GetName() }).IsOk();
			return bDeleted;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			if (bDeleted)
			{
				// The files go back to the default changelist; the next refresh shows them there.
				LodeProvider.RemoveChangelistFromCache(Changelist);
				return true;
			}
			return false;
		}

	private:
		FLodeChangelist Changelist;
		bool bDeleted = false;
	};

	class FEditChangelistWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "EditChangelist"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			// Lode 0.2 cannot change a changelist's description (proposed in unreal/docs/SPEC.md
			// section 5). The plugin keeps the new text for this session and submits with it.
			TSharedRef<FEditChangelist, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FEditChangelist>(Command.Operation);
			Changelist = Command.Changelist;
			Description = Operation->GetDescription().ToString();
			Command.InfoMessages.Add(LOCTEXT("EditLocal", "Lode cannot store a changed changelist description yet; the editor keeps it until it closes and submits with it."));
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			LodeProvider.GetChangelistStateInternal(Changelist)->Description = Description;
			return true;
		}

	private:
		FLodeChangelist Changelist;
		FString Description;
	};

	class FMoveToChangelistWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "MoveToChangelist"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const FString Target = Command.Changelist.IsInitialized() ? Command.Changelist.GetName() : FLodeChangelist::DefaultName;
			FLodeInvocation Move{ TEXT("changelist"), TEXT("move"), Target };
			Move.Paths = InWorkspace(Command, Command.Files);
			Move.bPathsFromFile = true;
			const bool bOk = Move.Paths.Num() > 0 && Command.Run(Move).IsOk();
			RunStatus(Command, Command.Files, false, Records);
			return bOk;
		}
	};

	class FShelveWorker : public FRecordsWorker
	{
	public:
		virtual FName GetName() const override { return "Shelve"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			TSharedRef<FShelve, ESPMode::ThreadSafe> Operation = StaticCastSharedRef<FShelve>(Command.Operation);
			FString Description = Operation->GetDescription().ToString().TrimStartAndEnd();
			FString Changelist = Command.Changelist.IsInitialized() ? Command.Changelist.GetName() : FLodeChangelist::DefaultName;
			TArray<FString> Files = InWorkspace(Command, FilesOrChangelist(Command));

			// A shelf comes from a named changelist, so the plugin can find it again. Shelving from
			// the default changelist first moves the files into a new one named after the description.
			if (Changelist == FLodeChangelist::DefaultName)
			{
				if (Files.Num() == 0)
				{
					Command.ErrorMessages.Add(LOCTEXT("ShelveNothing", "There is nothing to shelve."));
					return false;
				}
				if (Description.IsEmpty())
				{
					Description = TEXT("Shelved from the Unreal Editor");
				}
				const FLodeResult List = Command.Run({ TEXT("changelist"), TEXT("list") });
				if (!List.IsOk())
				{
					return false;
				}
				TArray<FString> Existing;
				for (const FLodeChangelistRecord& Record : LodeParsers::ParseChangelists(*List.Json, Command.Workspace.Root))
				{
					Existing.Add(Record.Name);
				}
				Changelist = LodeWorkers::MakeChangelistName(Description, Existing);
				if (!Command.Run({ TEXT("changelist"), TEXT("new"), Changelist, TEXT("-m"), Description }).IsOk())
				{
					return false;
				}
				FLodeInvocation Move{ TEXT("changelist"), TEXT("move"), Changelist };
				Move.Paths = Files;
				Move.bPathsFromFile = true;
				if (!Command.Run(Move).IsOk())
				{
					return false;
				}
			}

			// Keep the files here (as other providers do) and update the changelist's shelf when it has one.
			FLodeInvocation Shelve{ TEXT("shelve"), TEXT("-c"), Changelist, TEXT("--keep") };
			if (!Description.IsEmpty())
			{
				Shelve.Args.Append({ TEXT("-m"), Description });
			}
			if (ExistingShelf > 0)
			{
				Shelve.Args.Append({ TEXT("--update"), FString::FromInt(ExistingShelf) });
			}
			const FLodeResult Result = Command.Run(Shelve);
			if (!Result.IsOk())
			{
				return false;
			}
			Result.Json->TryGetNumberField(LodeJson::Shelf, ShelfId);
			Result.Json->TryGetNumberField(LodeJson::Version, ShelfVersion);
			ShelvedChangelist = Changelist;
			Command.InfoMessages.Add(FText::Format(LOCTEXT("Shelved", "Shelved as shelf {0} (version {1}). Others can unshelve it from Tools > Lode Shelves."), FText::AsNumber(ShelfId), FText::AsNumber(ShelfVersion)));
			RunStatus(Command, Files, false, Records);
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			if (ShelfId > 0)
			{
				LodeProvider.SetShelfForChangelist(ShelvedChangelist, ShelfId, ShelfVersion);
			}
			return FRecordsWorker::UpdateStates(LodeProvider);
		}

		/** Filled on the game thread before the command runs. */
		int32 ExistingShelf = 0;

	private:
		int32 ShelfId = 0;
		int32 ShelfVersion = 0;
		FString ShelvedChangelist;
	};

	class FUnshelveWorker : public FRecordsWorker
	{
	public:
		explicit FUnshelveWorker(FName InName) : Name(InName) {}

		virtual FName GetName() const override { return Name; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			int32 Id = ShelfId;
			if (Name == TEXT("LodeUnshelveShelf"))
			{
				Id = StaticCastSharedRef<FLodeUnshelveShelf>(Command.Operation)->ShelfId;
			}
			if (Id <= 0)
			{
				Command.ErrorMessages.Add(LOCTEXT("NoShelf", "This changelist has no shelf."));
				return false;
			}
			if (Command.Files.Num() > 0 && Name == TEXT("Unshelve"))
			{
				Command.InfoMessages.Add(LOCTEXT("UnshelveWhole", "Lode unshelves a whole shelf: every file in it comes back."));
			}
			const FLodeResult Result = Command.Run({ TEXT("unshelve"), FString::FromInt(Id) });
			if (!Result.IsOk())
			{
				return false;
			}
			TArray<FString> Touched;
			const TArray<TSharedPtr<FJsonValue>>* FilesJson = nullptr;
			if (Result.Json->TryGetArrayField(LodeJson::Files, FilesJson) && FilesJson)
			{
				for (const TSharedPtr<FJsonValue>& Value : *FilesJson)
				{
					const TSharedPtr<FJsonObject>* File = nullptr;
					FString Path;
					if (Value.IsValid() && Value->TryGetObject(File) && File && (*File)->TryGetStringField(LodeJson::Path, Path))
					{
						Touched.Add(LodePaths::ToFilename(Command.Workspace.Root, Path));
					}
				}
			}
			const TArray<FString> Conflicts = LodeParsers::ParseConflicts(*Result.Json, Command.Workspace.Root);
			Command.InfoMessages.Add(FText::Format(LOCTEXT("Unshelved", "Unshelved shelf {0}: {1} file(s)."), FText::AsNumber(Id), FText::AsNumber(Touched.Num())));
			if (Conflicts.Num() > 0)
			{
				Command.ErrorMessages.Add(FText::Format(LOCTEXT("UnshelveConflicts", "These files changed here too and are now in conflict: {0}. Resolve each to your version or the shelf's (right-click the asset, Lode)."),
					FText::FromString(JoinDepotPaths(Command, Conflicts))));
			}
			RunStatus(Command, Touched, false, Records);
			return true;
		}

		/** Filled on the game thread before the command runs. */
		int32 ShelfId = 0;

	private:
		FName Name;
	};

	class FDeleteShelvedWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "DeleteShelved"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			if (ShelfId <= 0)
			{
				Command.ErrorMessages.Add(LOCTEXT("NoShelfToDelete", "This changelist has no shelf."));
				return false;
			}
			if (Command.Files.Num() > 0 && Command.Files.Num() < ShelvedCount)
			{
				Command.ErrorMessages.Add(LOCTEXT("DeleteWholeShelf", "Lode deletes whole shelves. Select all the shelved files, or shelve again without the ones you want gone."));
				return false;
			}
			bDeleted = Command.Run({ TEXT("shelf"), TEXT("delete"), FString::FromInt(ShelfId) }).IsOk();
			ChangelistName = Command.Changelist.GetName();
			return bDeleted;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override
		{
			if (bDeleted)
			{
				LodeProvider.SetShelfForChangelist(ChangelistName, 0, 0);
				FLodeChangelistStateRef State = LodeProvider.GetChangelistStateInternal(FLodeChangelist(ChangelistName));
				State->ShelvedFiles.Reset();
				State->ShelfId = 0;
				return true;
			}
			return false;
		}

		/** Filled on the game thread before the command runs. */
		int32 ShelfId = 0;
		int32 ShelvedCount = 0;

	private:
		bool bDeleted = false;
		FString ChangelistName;
	};

	class FListShelvesWorker : public ILodeWorker
	{
	public:
		virtual FName GetName() const override { return "LodeListShelves"; }

		virtual bool Execute(FLodeCommand& Command) override
		{
			const FLodeResult Result = Command.Run({ TEXT("shelves"), TEXT("--state"), TEXT("open") });
			if (!Result.IsOk())
			{
				return false;
			}
			TArray<FLodeShelfRecord> Shelves = LodeParsers::ParseShelves(*Result.Json);
			for (FLodeShelfRecord& Shelf : Shelves)
			{
				const FLodeResult Show = Command.Run({ TEXT("shelf"), TEXT("show"), FString::FromInt(Shelf.Id) }, false);
				if (Show.IsOk())
				{
					const FLodeShelfRecord Detail = LodeParsers::ParseShelf(*Show.Json, Command.Workspace.Root);
					Shelf.Filenames = Detail.Filenames;
					Shelf.Actions = Detail.Actions;
				}
			}
			StaticCastSharedRef<FLodeListShelves>(Command.Operation)->Shelves = MoveTemp(Shelves);
			return true;
		}

		virtual bool UpdateStates(FLodeProvider& LodeProvider) const override { return false; }
	};
}

// ---------------------------------------------------------------------------------------------
// Registration. Some workers need provider data read on the game thread; the factories run there.
// ---------------------------------------------------------------------------------------------

namespace
{
	template <typename WorkerType>
	FLodeWorkerRef Make()
	{
		return MakeShared<WorkerType, ESPMode::ThreadSafe>();
	}
}

void LodeWorkers::RegisterAll(FLodeProvider& InProvider)
{
	InProvider.RegisterWorker("Connect", FGetLodeWorker::CreateStatic(&Make<FConnectWorker>));
	InProvider.RegisterWorker("LodeLogin", FGetLodeWorker::CreateStatic(&Make<FLoginWorker>));
	InProvider.RegisterWorker("UpdateStatus", FGetLodeWorker::CreateStatic(&Make<FUpdateStatusWorker>));
	InProvider.RegisterWorker("LodeRefreshServer", FGetLodeWorker::CreateStatic(&Make<FRefreshServerWorker>));
	InProvider.RegisterWorker("CheckOut", FGetLodeWorker::CreateStatic(&Make<FCheckOutWorker>));
	InProvider.RegisterWorker("MarkForAdd", FGetLodeWorker::CreateStatic(&Make<FMarkForAddWorker>));
	InProvider.RegisterWorker("Delete", FGetLodeWorker::CreateStatic(&Make<FDeleteWorker>));
	InProvider.RegisterWorker("Revert", FGetLodeWorker::CreateStatic(&Make<FRevertWorker>));
	InProvider.RegisterWorker("RevertUnchanged", FGetLodeWorker::CreateStatic(&Make<FRevertUnchangedWorker>));
	InProvider.RegisterWorker("Sync", FGetLodeWorker::CreateStatic(&Make<FSyncWorker>));
	InProvider.RegisterWorker("CheckIn", FGetLodeWorker::CreateStatic(&Make<FCheckInWorker>));
	InProvider.RegisterWorker("Copy", FGetLodeWorker::CreateStatic(&Make<FCopyWorker>));
	InProvider.RegisterWorker("Resolve", FGetLodeWorker::CreateLambda([]() -> FLodeWorkerRef { return MakeShared<FResolveWorker, ESPMode::ThreadSafe>(FName("Resolve")); }));
	InProvider.RegisterWorker("LodeResolve", FGetLodeWorker::CreateLambda([]() -> FLodeWorkerRef { return MakeShared<FResolveWorker, ESPMode::ThreadSafe>(FName("LodeResolve")); }));
	InProvider.RegisterWorker("NewChangelist", FGetLodeWorker::CreateStatic(&Make<FNewChangelistWorker>));
	InProvider.RegisterWorker("DeleteChangelist", FGetLodeWorker::CreateStatic(&Make<FDeleteChangelistWorker>));
	InProvider.RegisterWorker("EditChangelist", FGetLodeWorker::CreateStatic(&Make<FEditChangelistWorker>));
	InProvider.RegisterWorker("MoveToChangelist", FGetLodeWorker::CreateStatic(&Make<FMoveToChangelistWorker>));
	InProvider.RegisterWorker("LodeListShelves", FGetLodeWorker::CreateStatic(&Make<FListShelvesWorker>));
	InProvider.RegisterWorker("LodeUnshelveShelf", FGetLodeWorker::CreateLambda([]() -> FLodeWorkerRef { return MakeShared<FUnshelveWorker, ESPMode::ThreadSafe>(FName("LodeUnshelveShelf")); }));

	// Workers that read the shelf remembered for the command's changelist. The factory runs on
	// the game thread inside Execute, before the command is queued, but does not know the
	// changelist yet; the provider hands it over through PrepareWorker below.
	InProvider.RegisterWorker("UpdateChangelistsStatus", FGetLodeWorker::CreateLambda([&InProvider]() -> FLodeWorkerRef
	{
		TSharedRef<FUpdateChangelistsWorker, ESPMode::ThreadSafe> Worker = MakeShared<FUpdateChangelistsWorker, ESPMode::ThreadSafe>();
		for (const FSourceControlChangelistRef& Changelist : InProvider.GetChangelists(EStateCacheUsage::Use))
		{
			const FString& Name = static_cast<const FLodeChangelist&>(Changelist.Get()).GetName();
			const int32 Shelf = InProvider.GetShelfForChangelist(Name);
			if (Shelf > 0)
			{
				Worker->ShelfIds.Add(Name, Shelf);
			}
		}
		return Worker;
	}));
	InProvider.RegisterWorker("Shelve", FGetLodeWorker::CreateStatic(&Make<FShelveWorker>));
	InProvider.RegisterWorker("Unshelve", FGetLodeWorker::CreateLambda([]() -> FLodeWorkerRef { return MakeShared<FUnshelveWorker, ESPMode::ThreadSafe>(FName("Unshelve")); }));
	InProvider.RegisterWorker("DeleteShelved", FGetLodeWorker::CreateStatic(&Make<FDeleteShelvedWorker>));
}

void LodeWorkers::PrepareWorker(ILodeWorker& Worker, const FLodeProvider& InProvider, const FLodeChangelist& Changelist)
{
	const FName Name = Worker.GetName();
	int32 Version = 0;
	const int32 Shelf = Changelist.IsInitialized() ? InProvider.GetShelfForChangelist(Changelist.GetName(), &Version) : 0;
	if (Name == TEXT("Shelve"))
	{
		static_cast<FShelveWorker&>(Worker).ExistingShelf = Shelf;
	}
	else if (Name == TEXT("Unshelve"))
	{
		static_cast<FUnshelveWorker&>(Worker).ShelfId = Shelf;
	}
	else if (Name == TEXT("DeleteShelved"))
	{
		FDeleteShelvedWorker& DeleteWorker = static_cast<FDeleteShelvedWorker&>(Worker);
		DeleteWorker.ShelfId = Shelf;
		if (const TSharedPtr<FLodeChangelistState, ESPMode::ThreadSafe> State = InProvider.FindChangelistState(Changelist))
		{
			DeleteWorker.ShelvedCount = State->ShelvedFiles.Num();
		}
	}
}

#undef LOCTEXT_NAMESPACE
