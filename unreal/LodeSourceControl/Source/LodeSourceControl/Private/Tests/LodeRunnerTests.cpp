// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
// unreal T02: the command runner against the fake lode.

#include "LodeTestHelpers.h"

#if WITH_DEV_AUTOMATION_TESTS

#include "HAL/PlatformTime.h"

#include "LodeJsonFields.h"
#include "LodeRunner.h"

namespace
{
	struct FRunnerFixture
	{
		FString Dir = LodeTest::MakeTempDir(TEXT("Runner"));
		FString Log = Dir / TEXT("calls.jsonl");
		FLodeRunner Runner{ LodeTest::FakeConfig(TEXT("runner"), Log, Dir) };

		~FRunnerFixture() { LodeTest::RemoveDir(Dir); }

		FLodeResult Run(const FLodeInvocation& Invocation) const
		{
			const FLodeRunner& Local = Runner;
			return LodeTest::OffGameThread([&Local, Invocation]() { return Local.Run(Invocation); });
		}
	};
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerSuccessTest, "Lode.Runner.Success", LODE_TEST_FLAGS)
bool FLodeRunnerSuccessTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	if (!TestFalse(TEXT("Python for the fake lode is available"), LodeTest::FindPython().IsEmpty()))
	{
		return false;
	}
	const FLodeResult Result = Fixture.Run({ TEXT("--version") });
	TestTrue(TEXT("ok"), Result.IsOk());
	TestEqual(TEXT("exit code"), Result.ExitCode, 0);
	FString Version;
	TestTrue(TEXT("version field"), Result.Json.IsValid() && Result.Json->TryGetStringField(LodeJson::Version, Version));
	TestEqual(TEXT("version"), Version, FString(TEXT("0.2.0")));

	const TArray<TSharedPtr<FJsonObject>> Calls = LodeTest::ReadLog(Fixture.Log);
	TestEqual(TEXT("one call"), Calls.Num(), 1);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerExitCodesTest, "Lode.Runner.ExitCodes", LODE_TEST_FLAGS)
bool FLodeRunnerExitCodesTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;

	const FLodeResult Refused = Fixture.Run({ TEXT("refuse"), TEXT("x") });
	TestEqual(TEXT("exit 1 is Failed"), Refused.Outcome, ELodeOutcome::Failed);
	TestEqual(TEXT("error code"), Refused.Error.Code, FString(LodeErrorCode::OutOfDate));
	TestEqual(TEXT("error paths"), Refused.Error.Paths.Num(), 1);
	TestTrue(TEXT("message kept"), Refused.Describe().Contains(TEXT("update first")));

	TestEqual(TEXT("exit 2 is Usage"), Fixture.Run({ TEXT("usage") }).Outcome, ELodeOutcome::Usage);

	const FLodeResult Auth = Fixture.Run({ TEXT("auth") });
	TestEqual(TEXT("exit 3 is Auth"), Auth.Outcome, ELodeOutcome::Auth);
	TestTrue(TEXT("auth message says how to sign in"), Auth.Describe().Contains(TEXT("Sign in")));

	const FLodeResult Network = Fixture.Run({ TEXT("network") });
	TestEqual(TEXT("exit 4 is Network"), Network.Outcome, ELodeOutcome::Network);
	TestTrue(TEXT("network message says what to check"), Network.Describe().Contains(TEXT("server")));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerMalformedTest, "Lode.Runner.MalformedOutput", LODE_TEST_FLAGS)
bool FLodeRunnerMalformedTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	TestEqual(TEXT("not JSON"), Fixture.Run({ TEXT("garbage") }).Outcome, ELodeOutcome::BadOutput);
	TestEqual(TEXT("two documents"), Fixture.Run({ TEXT("two-documents") }).Outcome, ELodeOutcome::BadOutput);
	const FLodeResult Empty = Fixture.Run({ TEXT("empty") });
	TestEqual(TEXT("nothing printed"), Empty.Outcome, ELodeOutcome::BadOutput);
	TestTrue(TEXT("stderr kept"), Empty.StdErr.Contains(TEXT("lode crashed")));
	TestEqual(TEXT("ok with a failing exit code"), Fixture.Run({ TEXT("ok-but-failed") }).Outcome, ELodeOutcome::BadOutput);

	// The same rules without a process.
	TArray<uint8> Bytes;
	const FTCHARToUTF8 Text(TEXT("{\"ok\": false, \"error\": {\"code\": \"locked\", \"message\": \"m\", \"details\": {\"paths\": [\"a\", \"b\"]}}}"));
	Bytes.Append(reinterpret_cast<const uint8*>(Text.Get()), Text.Length());
	const FLodeResult Parsed = FLodeRunner::ParseOutput(1, Bytes, FString());
	TestEqual(TEXT("parsed code"), Parsed.Error.Code, FString(TEXT("locked")));
	TestEqual(TEXT("parsed paths"), Parsed.Error.Paths.Num(), 2);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerTimeoutTest, "Lode.Runner.Timeout", LODE_TEST_FLAGS)
bool FLodeRunnerTimeoutTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	FLodeInvocation Slow{ TEXT("slow") };
	Slow.TimeoutSeconds = 1.0;
	const double Start = FPlatformTime::Seconds();
	const FLodeResult Result = Fixture.Run(Slow);
	TestEqual(TEXT("timed out"), Result.Outcome, ELodeOutcome::Timeout);
	TestTrue(TEXT("stopped well before the fake would finish"), FPlatformTime::Seconds() - Start < 15.0);
	TestTrue(TEXT("says what to do"), Result.Describe().Contains(TEXT("connection")));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerCancelTest, "Lode.Runner.Cancel", LODE_TEST_FLAGS)
bool FLodeRunnerCancelTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	FLodeInvocation Slow{ TEXT("slow") };
	Slow.Cancel = MakeShared<FLodeCancelFlag, ESPMode::ThreadSafe>(false);
	const TSharedPtr<FLodeCancelFlag, ESPMode::ThreadSafe> Flag = Slow.Cancel;
	const FLodeRunner& Runner = Fixture.Runner;
	const double Start = FPlatformTime::Seconds();
	TFuture<FLodeResult> Future = Async(EAsyncExecution::Thread, [&Runner, Slow]() { return Runner.Run(Slow); });
	FPlatformProcess::Sleep(0.5f);
	Flag->store(true);
	const FLodeResult Result = Future.Get();
	TestEqual(TEXT("cancelled"), Result.Outcome, ELodeOutcome::Cancelled);
	TestTrue(TEXT("stopped promptly"), FPlatformTime::Seconds() - Start < 10.0);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerNoDeadlockTest, "Lode.Runner.NoDeadlock", LODE_TEST_FLAGS)
bool FLodeRunnerNoDeadlockTest::RunTest(const FString& Parameters)
{
	// 256 KiB on standard error before the answer: far more than a pipe holds.
	FRunnerFixture Fixture;
	FLodeInvocation Noisy{ TEXT("noisy") };
	Noisy.TimeoutSeconds = 30.0;
	const FLodeResult Result = Fixture.Run(Noisy);
	TestTrue(TEXT("finished"), Result.IsOk());
	TestTrue(TEXT("read all of stderr"), Result.StdErr.Len() >= 200000);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerStdInTest, "Lode.Runner.StdIn", LODE_TEST_FLAGS)
bool FLodeRunnerStdInTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	FLodeInvocation Echo{ TEXT("echo-stdin") };
	Echo.StdIn = TEXT("lode_test_token_Ü");
	TestTrue(TEXT("ok"), Fixture.Run(Echo).IsOk());
	const TArray<TSharedPtr<FJsonObject>> Calls = LodeTest::ReadLog(Fixture.Log);
	TestEqual(TEXT("standard input arrived as UTF-8"), Calls.Num() > 0 ? Calls[0]->GetStringField(TEXT("stdin")) : FString(), FString(TEXT("lode_test_token_Ü")));

	// Without input the fake sees an empty, closed standard input (and does not wait).
	TestTrue(TEXT("ok without input"), Fixture.Run({ TEXT("echo-stdin") }).IsOk());
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerUnicodeTest, "Lode.Runner.Unicode", LODE_TEST_FLAGS)
bool FLodeRunnerUnicodeTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	const FLodeResult Result = Fixture.Run({ TEXT("unicode") });
	FString Message;
	TestTrue(TEXT("ok"), Result.IsOk() && Result.Json->TryGetStringField(TEXT("message"), Message));
	TestEqual(TEXT("decoded"), Message, FString(TEXT("Ünïcödé ✓ 日本")));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerPathsFromTest, "Lode.Runner.PathsFromFile", LODE_TEST_FLAGS)
bool FLodeRunnerPathsFromTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	FLodeInvocation Status{ TEXT("status"), TEXT("--unchanged") };
	Status.bPathsFromFile = true;
	for (int32 Index = 0; Index < 5000; ++Index)
	{
		Status.Paths.Add(FString::Printf(TEXT("%s/Content/Some Folder/Asset_%05d.uasset"), *Fixture.Dir, Index));
	}
	TestTrue(TEXT("ok"), Fixture.Run(Status).IsOk());
	const TArray<TSharedPtr<FJsonObject>> Calls = LodeTest::ReadLog(Fixture.Log);
	TestEqual(TEXT("one process for 5000 paths"), Calls.Num(), 1);
	TestEqual(TEXT("every path arrived"), Calls.Num() == 1 ? Calls[0]->GetArrayField(TEXT("paths")).Num() : 0, 5000);

	TArray<FString> Left;
	IFileManager::Get().FindFiles(Left, *(Fixture.Dir / TEXT("tmp") / TEXT("*.txt")), true, false);
	TestEqual(TEXT("the path list file is removed"), Left.Num(), 0);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerBatchingTest, "Lode.Runner.Batching", LODE_TEST_FLAGS)
bool FLodeRunnerBatchingTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	// resolve takes no --paths-from: the runner must split the list.
	FLodeInvocation Resolve{ TEXT("resolve"), TEXT("--mine") };
	for (int32 Index = 0; Index < 1500; ++Index)
	{
		Resolve.Paths.Add(FString::Printf(TEXT("Content/A rather long folder name/Character_%05d/Mesh.uasset"), Index));
	}
	const FLodeRunner& Runner = Fixture.Runner;
	const TArray<FLodeResult> Results = LodeTest::OffGameThread([&Runner, Resolve]() { return Runner.RunBatched(Resolve); });
	TestTrue(TEXT("more than one call"), Results.Num() > 1);
	for (const FLodeResult& Result : Results)
	{
		TestTrue(TEXT("each call ok"), Result.IsOk());
	}

	int32 Seen = 0;
	for (const TSharedPtr<FJsonObject>& Call : LodeTest::ReadLog(Fixture.Log))
	{
		const TArray<TSharedPtr<FJsonValue>>& Args = Call->GetArrayField(TEXT("args"));
		Seen += Args.Num() - 2; // "resolve", "--mine"
	}
	TestEqual(TEXT("every path passed exactly once"), Seen, 1500);

	// Pure splitting rules.
	const TArray<TArray<FString>> Batches = FLodeRunner::SplitBatches({ TEXT("aaaa"), TEXT("bbbb"), TEXT("cccc") }, 11, 100);
	TestEqual(TEXT("split by length"), Batches.Num(), 2);
	TestEqual(TEXT("split by count"), FLodeRunner::SplitBatches({ TEXT("a"), TEXT("b"), TEXT("c") }, 1000, 2).Num(), 2);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerQuotingTest, "Lode.Runner.Quoting", LODE_TEST_FLAGS)
bool FLodeRunnerQuotingTest::RunTest(const FString& Parameters)
{
	TestEqual(TEXT("plain"), FLodeRunner::QuoteArgument(TEXT("status")), FString(TEXT("status")));
	TestEqual(TEXT("empty"), FLodeRunner::QuoteArgument(TEXT("")), FString(TEXT("\"\"")));
	TestEqual(TEXT("space"), FLodeRunner::QuoteArgument(TEXT("a b")), FString(TEXT("\"a b\"")));
#if PLATFORM_WINDOWS
	TestEqual(TEXT("quote"), FLodeRunner::QuoteArgument(TEXT("say \"hi\"")), FString(TEXT("\"say \\\"hi\\\"\"")));
	TestEqual(TEXT("trailing backslash"), FLodeRunner::QuoteArgument(TEXT("C:\\My Folder\\")), FString(TEXT("\"C:\\My Folder\\\\\"")));
#endif
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerRefusesGameThreadTest, "Lode.Runner.RefusesGameThread", LODE_TEST_FLAGS)
bool FLodeRunnerRefusesGameThreadTest::RunTest(const FString& Parameters)
{
	FRunnerFixture Fixture;
	AddExpectedError(TEXT("lode must never run on the game thread"), EAutomationExpectedErrorFlags::Contains, 1);
	const FLodeResult Result = Fixture.Runner.Run({ TEXT("--version") });
	TestFalse(TEXT("refused"), Result.IsOk());
	TestEqual(TEXT("nothing started"), LodeTest::ReadLog(Fixture.Log).Num(), 0);
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeRunnerNotFoundTest, "Lode.Runner.NotFound", LODE_TEST_FLAGS)
bool FLodeRunnerNotFoundTest::RunTest(const FString& Parameters)
{
	FLodeRunnerConfig Config;
	Config.Executable = FPaths::ProjectIntermediateDir() / TEXT("no-such-lode.exe");
	const FLodeRunner Runner(Config);
	const FLodeResult Result = LodeTest::OffGameThread([&Runner]() { return Runner.Run({ TEXT("--version") }); });
	TestEqual(TEXT("not started"), Result.Outcome, ELodeOutcome::NotStarted);
	TestTrue(TEXT("says how to fix it"), Result.Describe().Contains(TEXT("Install Lode")));
	return true;
}

#endif
