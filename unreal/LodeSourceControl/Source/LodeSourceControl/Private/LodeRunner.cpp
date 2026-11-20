// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeRunner.h"

#include "HAL/FileManager.h"
#include "HAL/PlatformMisc.h"
#include "HAL/PlatformProcess.h"
#include "HAL/PlatformTime.h"
#include "Misc/FileHelper.h"
#include "Misc/Paths.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"

#include "LodeJsonFields.h"
#include "LodeLog.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl"

namespace
{
	FString JoinForLog(const TArray<FString>& Arguments)
	{
		// Arguments never carry secrets (tokens go through standard input), so they can be logged.
		return FString::Join(Arguments, TEXT(" "));
	}

	/** Deletes a file when it goes out of scope. */
	struct FScopedTempFile
	{
		FString Path;
		~FScopedTempFile()
		{
			if (!Path.IsEmpty())
			{
				IFileManager::Get().Delete(*Path, false, true, true);
			}
		}
	};
}

FString FLodeResult::Describe() const
{
	switch (Outcome)
	{
	case ELodeOutcome::Success:
		return TEXT("Done.");
	case ELodeOutcome::NotStarted:
		return Error.Message.IsEmpty()
			? FString(TEXT("The lode program could not be started. Install Lode, or set the path to lode in the Lode revision control settings."))
			: Error.Message;
	case ELodeOutcome::Timeout:
		return FString::Printf(TEXT("lode did not finish in time and was stopped after %.0f seconds. Check the connection to the Lode server and try again."), Seconds);
	case ELodeOutcome::Cancelled:
		return TEXT("Cancelled.");
	case ELodeOutcome::BadOutput:
		return FString::Printf(TEXT("lode printed something the plugin cannot read (exit code %d). Check that lode is version 0.2 or later. %s"), ExitCode, *StdErr.TrimStartAndEnd());
	default:
		break;
	}

	FString Text = Error.Message.IsEmpty() ? StdErr.TrimStartAndEnd() : Error.Message;
	if (Text.IsEmpty())
	{
		Text = FString::Printf(TEXT("lode failed with exit code %d."), ExitCode);
	}
	if (Outcome == ELodeOutcome::Auth && Error.Code == LodeErrorCode::Unauthenticated)
	{
		Text += TEXT(" Sign in from the Lode revision control settings, or run `lode login` in a terminal.");
	}
	else if (Outcome == ELodeOutcome::Network)
	{
		Text += TEXT(" Check that the Lode server is running and reachable.");
	}
	return Text;
}

FString FLodeRunner::QuoteArgument(const FString& Argument)
{
	if (!Argument.IsEmpty() && !Argument.Contains(TEXT(" ")) && !Argument.Contains(TEXT("\t")) && !Argument.Contains(TEXT("\"")))
	{
		return Argument;
	}

#if PLATFORM_WINDOWS
	// Rules of CommandLineToArgvW and the Rust standard library: backslashes are literal unless
	// they come before a quote; then each one is doubled and the quote is escaped.
	FString Out = TEXT("\"");
	int32 Backslashes = 0;
	for (const TCHAR Char : Argument)
	{
		if (Char == TEXT('\\'))
		{
			++Backslashes;
			continue;
		}
		if (Char == TEXT('"'))
		{
			Out += FString::ChrN(Backslashes * 2 + 1, TEXT('\\'));
			Out += TEXT('"');
		}
		else
		{
			Out += FString::ChrN(Backslashes, TEXT('\\'));
			Out += Char;
		}
		Backslashes = 0;
	}
	Out += FString::ChrN(Backslashes * 2, TEXT('\\'));
	Out += TEXT("\"");
	return Out;
#else
	// The Linux and macOS editors split the command line at spaces and trim surrounding quotes;
	// they know no escapes. Free text therefore loses inner quotes and repeated spaces here, which
	// is why the plugin passes messages through files wherever lode accepts one.
	FString Clean = Argument.Replace(TEXT("\""), TEXT("'"));
	while (Clean.Contains(TEXT("  ")))
	{
		Clean.ReplaceInline(TEXT("  "), TEXT(" "));
	}
	return FString::Printf(TEXT("\"%s\""), *Clean);
#endif
}

FString FLodeRunner::BuildCommandLine(const TArray<FString>& Arguments)
{
	TArray<FString> Quoted;
	Quoted.Reserve(Arguments.Num());
	for (const FString& Argument : Arguments)
	{
		Quoted.Add(QuoteArgument(Argument));
	}
	return FString::Join(Quoted, TEXT(" "));
}

TArray<TArray<FString>> FLodeRunner::SplitBatches(const TArray<FString>& Paths, int32 CharacterBudget, int32 ArgumentBudget)
{
	TArray<TArray<FString>> Batches;
	TArray<FString> Current;
	int32 CurrentLength = 0;
	for (const FString& Path : Paths)
	{
		const int32 Cost = QuoteArgument(Path).Len() + 1;
		if (Current.Num() > 0 && (CurrentLength + Cost > CharacterBudget || Current.Num() >= ArgumentBudget))
		{
			Batches.Add(MoveTemp(Current));
			Current.Reset();
			CurrentLength = 0;
		}
		Current.Add(Path);
		CurrentLength += Cost;
	}
	if (Current.Num() > 0)
	{
		Batches.Add(MoveTemp(Current));
	}
	return Batches;
}

FLodeResult FLodeRunner::ParseOutput(int32 ExitCode, const TArray<uint8>& StdOut, const FString& StdErr)
{
	FLodeResult Result;
	Result.ExitCode = ExitCode;
	Result.StdErr = StdErr;

	FUTF8ToTCHAR Converted(reinterpret_cast<const ANSICHAR*>(StdOut.GetData()), StdOut.Num());
	FString Text(Converted.Length(), Converted.Get());
	Text.TrimStartAndEndInline();

	TSharedPtr<FJsonObject> Json;
	if (!Text.IsEmpty())
	{
		const TSharedRef<TJsonReader<>> Reader = TJsonReaderFactory<>::Create(Text);
		if (!FJsonSerializer::Deserialize(Reader, Json))
		{
			Json.Reset();
		}
	}

	if (!Json.IsValid())
	{
		Result.Outcome = ELodeOutcome::BadOutput;
		Result.Error.Message = StdErr.TrimStartAndEnd();
		return Result;
	}

	Result.Json = Json;
	bool bOk = false;
	if (!Json->TryGetBoolField(LodeJson::Ok, bOk))
	{
		Result.Outcome = ELodeOutcome::BadOutput;
		return Result;
	}

	if (bOk)
	{
		// A document that says ok with a failing exit code breaks Lode's promise; trust neither.
		Result.Outcome = ExitCode == 0 ? ELodeOutcome::Success : ELodeOutcome::BadOutput;
		return Result;
	}

	const TSharedPtr<FJsonObject>* ErrorObject = nullptr;
	if (Json->TryGetObjectField(LodeJson::Error, ErrorObject) && ErrorObject && ErrorObject->IsValid())
	{
		(*ErrorObject)->TryGetStringField(LodeJson::Code, Result.Error.Code);
		(*ErrorObject)->TryGetStringField(LodeJson::Message, Result.Error.Message);
		const TSharedPtr<FJsonObject>* Details = nullptr;
		if ((*ErrorObject)->TryGetObjectField(LodeJson::Details, Details) && Details && Details->IsValid())
		{
			Result.Error.Details = *Details;
			(*Details)->TryGetStringArrayField(LodeJson::Paths, Result.Error.Paths);
		}
	}

	switch (ExitCode)
	{
	case 1: Result.Outcome = ELodeOutcome::Failed; break;
	case 2: Result.Outcome = ELodeOutcome::Usage; break;
	case 3: Result.Outcome = ELodeOutcome::Auth; break;
	case 4: Result.Outcome = ELodeOutcome::Network; break;
	default: Result.Outcome = ELodeOutcome::Failed; break;
	}
	return Result;
}

FString FLodeRunner::ResolveExecutable(const FString& Setting)
{
	const FString Trimmed = Setting.TrimStartAndEnd().TrimQuotes();
	if (!Trimmed.IsEmpty() && (Trimmed.Contains(TEXT("/")) || Trimmed.Contains(TEXT("\\"))))
	{
		return FPaths::FileExists(Trimmed) ? FPaths::ConvertRelativePathToFull(Trimmed) : FString();
	}

	const FString Name = Trimmed.IsEmpty() ? FString(TEXT("lode")) : Trimmed;
	TArray<FString> Candidates;
#if PLATFORM_WINDOWS
	Candidates.Add(Name.EndsWith(TEXT(".exe")) ? Name : Name + TEXT(".exe"));
#else
	Candidates.Add(Name);
#endif

	const FString PathVariable = FPlatformMisc::GetEnvironmentVariable(TEXT("PATH"));
	TArray<FString> Directories;
	PathVariable.ParseIntoArray(Directories, FPlatformMisc::GetPathVarDelimiter(), true);
#if PLATFORM_WINDOWS
	// Where the Lode installer puts lode.exe, in case PATH was not refreshed for this editor.
	const FString ProgramFiles = FPlatformMisc::GetEnvironmentVariable(TEXT("ProgramFiles"));
	if (!ProgramFiles.IsEmpty())
	{
		Directories.Add(FPaths::Combine(ProgramFiles, TEXT("Lode")));
	}
	const FString LocalAppData = FPlatformMisc::GetEnvironmentVariable(TEXT("LOCALAPPDATA"));
	if (!LocalAppData.IsEmpty())
	{
		Directories.Add(FPaths::Combine(LocalAppData, TEXT("Programs"), TEXT("Lode")));
	}
#else
	Directories.Add(TEXT("/usr/local/bin"));
	Directories.Add(TEXT("/opt/homebrew/bin"));
	const FString Home = FPlatformMisc::GetEnvironmentVariable(TEXT("HOME"));
	if (!Home.IsEmpty())
	{
		Directories.Add(FPaths::Combine(Home, TEXT(".cargo"), TEXT("bin")));
		Directories.Add(FPaths::Combine(Home, TEXT(".local"), TEXT("bin")));
	}
#endif

	for (const FString& Directory : Directories)
	{
		for (const FString& Candidate : Candidates)
		{
			const FString Full = FPaths::Combine(Directory.TrimQuotes(), Candidate);
			if (FPaths::FileExists(Full))
			{
				return Full;
			}
		}
	}
	return FString();
}

FLodeResult FLodeRunner::Run(const FLodeInvocation& Invocation) const
{
	if (IsInGameThread())
	{
		UE_LOG(LogLodeSourceControl, Error, TEXT("lode must never run on the game thread (lode %s)"), Invocation.Args.Num() > 0 ? *Invocation.Args[0] : TEXT(""));
		FLodeResult Refused;
		Refused.Outcome = ELodeOutcome::Usage;
		Refused.Error.Code = TEXT("game_thread");
		Refused.Error.Message = TEXT("Internal error: lode was called on the editor's main thread.");
		return Refused;
	}

	TArray<FString> Arguments = Config.LeadingArgs;
	Arguments.Append(Invocation.Args);
	Arguments.Add(TEXT("--json"));

	FScopedTempFile PathsFile;
	if (Invocation.Paths.Num() > 0)
	{
		if (Invocation.bPathsFromFile)
		{
			const FString Directory = Config.TempDirectory.IsEmpty() ? FPaths::ProjectIntermediateDir() / TEXT("LodeSourceControl") : Config.TempDirectory;
			IFileManager::Get().MakeDirectory(*Directory, true);
			PathsFile.Path = FPaths::ConvertRelativePathToFull(FPaths::CreateTempFilename(*Directory, TEXT("lode-paths-"), TEXT(".txt")));
			const FString Lines = FString::Join(Invocation.Paths, TEXT("\n")) + TEXT("\n");
			if (!FFileHelper::SaveStringToFile(Lines, *PathsFile.Path, FFileHelper::EEncodingOptions::ForceUTF8WithoutBOM))
			{
				FLodeResult Failed;
				Failed.Outcome = ELodeOutcome::NotStarted;
				Failed.Error.Message = FString::Printf(TEXT("Could not write the list of files to %s."), *PathsFile.Path);
				return Failed;
			}
			Arguments.Add(TEXT("--paths-from"));
			Arguments.Add(PathsFile.Path);
		}
		else
		{
			Arguments.Add(TEXT("--"));
			Arguments.Append(Invocation.Paths);
		}
	}

	return RunProcess(Arguments, Invocation);
}

TArray<FLodeResult> FLodeRunner::RunBatched(const FLodeInvocation& Invocation) const
{
	TArray<FLodeResult> Results;
	if (Invocation.bPathsFromFile || Invocation.Paths.Num() == 0)
	{
		Results.Add(Run(Invocation));
		return Results;
	}

	const int32 FixedLength = BuildCommandLine(Config.LeadingArgs).Len() + BuildCommandLine(Invocation.Args).Len() + 16;
	const int32 FixedArguments = Config.LeadingArgs.Num() + Invocation.Args.Num() + 2;
	const TArray<TArray<FString>> Batches = SplitBatches(
		Invocation.Paths,
		FMath::Max(1024, Config.MaxCommandLineLength - FixedLength),
		FMath::Max(1, Config.MaxArguments - FixedArguments));

	for (const TArray<FString>& Batch : Batches)
	{
		FLodeInvocation One = Invocation;
		One.Paths = Batch;
		Results.Add(Run(One));
		if (!Results.Last().IsOk())
		{
			break;
		}
	}
	return Results;
}

FLodeResult FLodeRunner::RunProcess(const TArray<FString>& Arguments, const FLodeInvocation& Invocation) const
{
	const double Start = FPlatformTime::Seconds();
	FLodeResult Result;

	if (Config.Executable.IsEmpty() || !FPaths::FileExists(Config.Executable))
	{
		Result.Outcome = ELodeOutcome::NotStarted;
		Result.Error.Message = Config.Executable.IsEmpty()
			? FString(TEXT("The lode program was not found on PATH. Install Lode, or set the path to lode in the Lode revision control settings."))
			: FString::Printf(TEXT("The lode program was not found at %s. Install Lode, or correct the path in the Lode revision control settings."), *Config.Executable);
		return Result;
	}

	void* StdOutRead = nullptr;
	void* StdOutWrite = nullptr;
	void* StdErrRead = nullptr;
	void* StdErrWrite = nullptr;
	void* StdInRead = nullptr;
	void* StdInWrite = nullptr;
	if (!FPlatformProcess::CreatePipe(StdOutRead, StdOutWrite)
		|| !FPlatformProcess::CreatePipe(StdErrRead, StdErrWrite)
		|| !FPlatformProcess::CreatePipe(StdInRead, StdInWrite, /*bWritePipeLocal*/ true))
	{
		FPlatformProcess::ClosePipe(StdOutRead, StdOutWrite);
		FPlatformProcess::ClosePipe(StdErrRead, StdErrWrite);
		FPlatformProcess::ClosePipe(StdInRead, StdInWrite);
		Result.Outcome = ELodeOutcome::NotStarted;
		Result.Error.Message = TEXT("Could not create pipes to talk to lode.");
		return Result;
	}

	const FString CommandLine = BuildCommandLine(Arguments);
	UE_LOG(LogLodeSourceControl, Verbose, TEXT("Running: %s %s"), *Config.Executable, *JoinForLog(Arguments));

	FProcHandle Process = FPlatformProcess::CreateProc(
		*Config.Executable, *CommandLine,
		/*bLaunchDetached*/ false, /*bLaunchHidden*/ true, /*bLaunchReallyHidden*/ true,
		/*OutProcessID*/ nullptr, /*PriorityModifier*/ 0,
		Config.WorkingDirectory.IsEmpty() ? nullptr : *Config.WorkingDirectory,
		/*PipeWriteChild*/ StdOutWrite, /*PipeReadChild*/ StdInRead, /*PipeStdErrChild*/ StdErrWrite);

	if (!Process.IsValid())
	{
		FPlatformProcess::ClosePipe(StdOutRead, StdOutWrite);
		FPlatformProcess::ClosePipe(StdErrRead, StdErrWrite);
		FPlatformProcess::ClosePipe(StdInRead, StdInWrite);
		Result.Outcome = ELodeOutcome::NotStarted;
		Result.Error.Message = FString::Printf(TEXT("Could not start %s."), *Config.Executable);
		return Result;
	}

	// Standard input: write what there is (a token or password is a few hundred bytes, far below
	// any pipe buffer, so this cannot block), then close it so lode never waits for a person.
	if (!Invocation.StdIn.IsEmpty())
	{
		FTCHARToUTF8 Utf8(*Invocation.StdIn);
		int32 Written = 0;
		FPlatformProcess::WritePipe(StdInWrite, reinterpret_cast<const uint8*>(Utf8.Get()), Utf8.Length(), &Written);
	}
	FPlatformProcess::ClosePipe(nullptr, StdInWrite);
	StdInWrite = nullptr;

	TArray<uint8> StdOut;
	FString StdErr;
	bool bStopped = false;

	auto Drain = [&]() -> bool
	{
		bool bRead = false;
		TArray<uint8> Chunk;
		if (FPlatformProcess::ReadPipeToArray(StdOutRead, Chunk) && Chunk.Num() > 0)
		{
			StdOut.Append(Chunk);
			bRead = true;
		}
		const FString ErrChunk = FPlatformProcess::ReadPipe(StdErrRead);
		if (!ErrChunk.IsEmpty())
		{
			StdErr += ErrChunk;
			bRead = true;
		}
		return bRead;
	};

	while (true)
	{
		const bool bRead = Drain();
		if (!FPlatformProcess::IsProcRunning(Process))
		{
			// Read what is still buffered after the process ended.
			while (Drain())
			{
			}
			break;
		}
		if (Invocation.Cancel.IsValid() && Invocation.Cancel->load())
		{
			Result.Outcome = ELodeOutcome::Cancelled;
			bStopped = true;
		}
		else if (FPlatformTime::Seconds() - Start > Invocation.TimeoutSeconds)
		{
			Result.Outcome = ELodeOutcome::Timeout;
			bStopped = true;
		}
		if (bStopped)
		{
			FPlatformProcess::TerminateProc(Process, /*KillTree*/ true);
			FPlatformProcess::WaitForProc(Process);
			break;
		}
		if (!bRead)
		{
			FPlatformProcess::Sleep(0.002f);
		}
	}

	int32 ExitCode = -1;
	if (!bStopped)
	{
		FPlatformProcess::GetProcReturnCode(Process, &ExitCode);
	}
	FPlatformProcess::CloseProc(Process);
	FPlatformProcess::ClosePipe(StdOutRead, StdOutWrite);
	FPlatformProcess::ClosePipe(StdErrRead, StdErrWrite);
	FPlatformProcess::ClosePipe(StdInRead, nullptr);

	if (bStopped)
	{
		Result.ExitCode = ExitCode;
		Result.StdErr = StdErr;
		Result.Seconds = FPlatformTime::Seconds() - Start;
		return Result;
	}

	Result = ParseOutput(ExitCode, StdOut, StdErr);
	Result.Seconds = FPlatformTime::Seconds() - Start;
	UE_LOG(LogLodeSourceControl, Verbose, TEXT("lode %s: exit %d in %.3fs"), Invocation.Args.Num() > 0 ? *Invocation.Args[0] : TEXT(""), ExitCode, Result.Seconds);
	if (!Result.IsOk())
	{
		UE_LOG(LogLodeSourceControl, Log, TEXT("lode %s failed: [%s] %s"), *JoinForLog(Invocation.Args), *Result.Error.Code, *Result.Describe());
	}
	return Result;
}

#undef LOCTEXT_NAMESPACE
