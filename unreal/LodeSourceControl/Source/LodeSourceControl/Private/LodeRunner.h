// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"
#include <atomic>

/** How a `lode` call ended. Exit codes 1 to 4 are Lode's own (CLI-JSON "Exit codes"). */
enum class ELodeOutcome : uint8
{
	Success,      // exit 0, "ok": true
	Failed,       // exit 1: the operation was refused (out_of_date, locked, ...)
	Usage,        // exit 2: the plugin called lode wrongly, or a path was invalid
	Auth,         // exit 3: not signed in, or not allowed
	Network,      // exit 4: server unreachable or server error
	NotStarted,   // the lode program could not be started
	Timeout,      // the time limit passed; the process was stopped
	Cancelled,    // cancelled by the editor; the process was stopped
	BadOutput,    // the output was not the one JSON document lode promises
};

/** A shared flag that a running call checks; set it from any thread to stop the call. */
using FLodeCancelFlag = std::atomic<bool>;

struct FLodeError
{
	FString Code;
	FString Message;
	TArray<FString> Paths;
	TSharedPtr<FJsonObject> Details;
};

struct FLodeResult
{
	ELodeOutcome Outcome = ELodeOutcome::NotStarted;
	int32 ExitCode = -1;
	/** The whole document on success, and also on a refusal (it then holds "error"). */
	TSharedPtr<FJsonObject> Json;
	FLodeError Error;
	FString StdErr;
	double Seconds = 0.0;

	bool IsOk() const { return Outcome == ELodeOutcome::Success && Json.IsValid(); }
	bool HasCode(const TCHAR* InCode) const { return Error.Code == InCode; }

	/** A sentence for a person: what went wrong and, where possible, what to do. */
	FString Describe() const;
};

/** One `lode` call. `--json` is added by the runner. */
struct FLodeInvocation
{
	/** Command and options, such as {"status", "--unchanged"}. */
	TArray<FString> Args;
	/** Paths, absolute or relative to the workspace. */
	TArray<FString> Paths;
	/** The command accepts `--paths-from FILE` (CLI-JSON "Many paths"). */
	bool bPathsFromFile = false;
	/** Sent to standard input as UTF-8, then standard input is closed. Never logged. */
	FString StdIn;
	/** Seconds before the process is stopped. */
	double TimeoutSeconds = 120.0;
	/** Optional: set to true from any thread to stop the call. */
	TSharedPtr<FLodeCancelFlag, ESPMode::ThreadSafe> Cancel;

	FLodeInvocation() = default;
	FLodeInvocation(std::initializer_list<FString> InArgs) : Args(InArgs) {}
};

struct FLodeRunnerConfig
{
	/** Full path of the lode program (or of a program that stands in for it in tests). */
	FString Executable;
	/** Arguments placed before Lode's own, such as the script path when the fake runs under Python. */
	TArray<FString> LeadingArgs;
	/** The workspace root; lode runs there. */
	FString WorkingDirectory;
	/** Where path lists for --paths-from are written. */
	FString TempDirectory;
	/** Longest command line built before a path list is split into batches. */
	int32 MaxCommandLineLength = 24000;
	/** Most arguments in one call (the Linux and macOS editors pass at most 256). */
	int32 MaxArguments = 200;
};

/**
 * Starts `lode <args> --json`, feeds standard input, reads standard output and standard error
 * without blocking, and parses the one JSON document lode prints. Never call it on the game
 * thread: a call can take as long as the server needs.
 */
class FLodeRunner
{
public:
	explicit FLodeRunner(FLodeRunnerConfig InConfig) : Config(MoveTemp(InConfig)) {}

	const FLodeRunnerConfig& GetConfig() const { return Config; }

	/** Runs one call. Paths go into a --paths-from file when the command takes one, otherwise on the command line. */
	FLodeResult Run(const FLodeInvocation& Invocation) const;

	/**
	 * Runs a call whose path list may be long. Commands that take --paths-from get the whole list
	 * in one call; others are split into batches under the command line limits, stopping at the
	 * first batch that fails. Returns one result per call made.
	 */
	TArray<FLodeResult> RunBatched(const FLodeInvocation& Invocation) const;

	/** Quotes one argument for a command line (Windows rules; the Linux and macOS editors trim the quotes). */
	static FString QuoteArgument(const FString& Argument);

	/** Joins arguments into one command line. */
	static FString BuildCommandLine(const TArray<FString>& Arguments);

	/** Turns what lode printed into a result. Public for tests. */
	static FLodeResult ParseOutput(int32 ExitCode, const TArray<uint8>& StdOut, const FString& StdErr);

	/** Splits paths into batches so each command line stays under the budget. Public for tests. */
	static TArray<TArray<FString>> SplitBatches(const TArray<FString>& Paths, int32 CharacterBudget, int32 ArgumentBudget);

	/** Finds the lode program: the setting when it names a file, otherwise `lode` on PATH. Empty when not found. */
	static FString ResolveExecutable(const FString& Setting);

private:
	FLodeResult RunProcess(const TArray<FString>& Arguments, const FLodeInvocation& Invocation) const;

	FLodeRunnerConfig Config;
};
