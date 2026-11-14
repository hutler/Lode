// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#if WITH_DEV_AUTOMATION_TESTS

#include "LodeTestHelpers.h"

#include "Misc/ScopeExit.h"
#include "SourceControlOperations.h"

#include "LodeOperations.h"
#include "LodePaths.h"
#include "LodeProvider.h"

/**
 * unreal T03 to T05: a provider of its own (not the editor's) driven against the fake lode with
 * Tests/FakeLode/scenarios/provider.json. Checks the lode commands sent and the states that result.
 */

namespace
{
	struct FProviderFixture
	{
		FString Root;
		FString LogPath;
		FLodeProvider Provider;

		FProviderFixture()
		{
			Root = LodeTest::MakeTempDir(TEXT("provider"));
			LogPath = Root / TEXT("tmp") / TEXT("calls.jsonl");
			IFileManager::Get().MakeDirectory(*(Root / TEXT(".lode")), true);
			IFileManager::Get().MakeDirectory(*(Root / TEXT("tmp")), true);
			for (const TCHAR* File : { TEXT("Content/Hero/Hero.uasset"), TEXT("Content/Maps/Main.umap"), TEXT("Config/DefaultGame.ini") })
			{
				FFileHelper::SaveStringToFile(TEXT("data"), *(Root / File));
			}
			Provider.SetRunnerOverride(LodeTest::FakeConfig(TEXT("provider"), LogPath, Root));
			Provider.SetWorkspaceRootOverride(Root);
			LodeWorkers::RegisterAll(Provider);
		}

		~FProviderFixture()
		{
			Provider.Close();
			LodeTest::RemoveDir(Root);
		}

		FString File(const TCHAR* Relative) const { return LodePaths::Normalize(Root / Relative); }

		/** Runs an operation the way the editor does without blocking: queued, then ticked. */
		ECommandResult::Type Run(const FSourceControlOperationRef& Operation, const TArray<FString>& Files)
		{
			ECommandResult::Type Result = ECommandResult::Failed;
			bool bDone = false;
			Provider.Execute(Operation, FSourceControlChangelistPtr(), Files, EConcurrency::Asynchronous,
				FSourceControlOperationComplete::CreateLambda([&](const FSourceControlOperationRef&, ECommandResult::Type InResult)
				{
					Result = InResult;
					bDone = true;
				}));
			Provider.WaitForCommands(30.0);
			return bDone ? Result : ECommandResult::Failed;
		}

		/** Each lode call, as "args joined by spaces" with the root shortened to {root}. */
		TArray<FString> Calls() const
		{
			TArray<FString> Out;
			for (const TSharedPtr<FJsonObject>& Call : LodeTest::ReadLog(LogPath))
			{
				TArray<FString> Args;
				Call->TryGetStringArrayField(TEXT("args"), Args);
				FString Line = FString::Join(Args, TEXT(" "));
				Line.ReplaceInline(*Root.Replace(TEXT("\\"), TEXT("/")), TEXT("{root}"));
				Line.ReplaceInline(*Root, TEXT("{root}"));
				Out.Add(Line);
			}
			return Out;
		}

		FLodeStateRef State(const TCHAR* Relative) { return Provider.GetStateInternal(File(Relative)); }
	};
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeProviderConnectTest, "Lode.Provider.Connect", LODE_TEST_FLAGS)
bool FLodeProviderConnectTest::RunTest(const FString& Parameters)
{
	FProviderFixture Fixture;
	Fixture.Provider.Init(true);
	Fixture.Provider.WaitForCommands(30.0);

	TestTrue(TEXT("available"), Fixture.Provider.IsAvailable());
	TestTrue(TEXT("no problem"), Fixture.Provider.GetProblem().IsEmpty());
	const FLodeWorkspaceInfo& Info = Fixture.Provider.GetWorkspace();
	TestEqual(TEXT("user"), Info.UserName, TEXT("alice"));
	TestEqual(TEXT("server"), Info.Server, TEXT("http://127.0.0.1:7480/"));
	TestEqual(TEXT("depot"), Info.Depot, TEXT("game"));
	TestEqual(TEXT("branch"), Info.Branch, TEXT("main"));
	TestEqual(TEXT("rev"), Info.Rev, 3);
	TestEqual(TEXT("version"), Info.ClientVersion, TEXT("0.2.0"));
	TestTrue(TEXT("changed file known after connect"), Fixture.State(TEXT("Config/DefaultGame.ini"))->IsModified());

	const TArray<FString> Calls = Fixture.Calls();
	TestTrue(TEXT("connect asks four things"), Calls == TArray<FString>{ TEXT("--version"), TEXT("status"), TEXT("whoami"), TEXT("log -n 1") });
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeProviderStatusTest, "Lode.Provider.Status", LODE_TEST_FLAGS)
bool FLodeProviderStatusTest::RunTest(const FString& Parameters)
{
	FProviderFixture Fixture;
	Fixture.Provider.Init(true);
	Fixture.Provider.WaitForCommands(30.0);

	TSharedRef<FUpdateStatus> Update = ISourceControlOperation::Create<FUpdateStatus>();
	Update->SetUpdateHistory(true);
	const TArray<FString> Files = { Fixture.File(TEXT("Content/Hero/Hero.uasset")), Fixture.File(TEXT("Content/Maps/Main.umap")), Fixture.File(TEXT("Saved/x.log")) };
	TestEqual(TEXT("status succeeds"), Fixture.Run(Update, Files), ECommandResult::Succeeded);

	FString Who;
	TestTrue(TEXT("Hero is locked by someone else"), Fixture.State(TEXT("Content/Hero/Hero.uasset"))->IsCheckedOutOther(&Who));
	TestEqual(TEXT("bob"), Who, TEXT("bob"));
	const FLodeStateRef Main = Fixture.State(TEXT("Content/Maps/Main.umap"));
	TestTrue(TEXT("Main can be checked out"), Main->CanCheckout());
	TestFalse(TEXT("Main is behind the server"), Main->IsCurrent());
	TestTrue(TEXT("history fetched"), Main->GetHistorySize() > 0);
	TestFalse(TEXT("Saved/x.log is not controlled"), Fixture.State(TEXT("Saved/x.log"))->IsSourceControlled());

	// Paths go to lode through --paths-from (the fake logs them after the arguments).
	const TArray<FString> Calls = Fixture.Calls();
	TestTrue(TEXT("one status call for all files"), Calls.ContainsByPredicate([](const FString& Call)
	{
		return Call.StartsWith(TEXT("status --unchanged")) && Call.Contains(TEXT("{root}/Content/Hero/Hero.uasset")) && Call.Contains(TEXT("{root}/Content/Maps/Main.umap"));
	}));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeProviderCheckOutTest, "Lode.Provider.CheckOut", LODE_TEST_FLAGS)
bool FLodeProviderCheckOutTest::RunTest(const FString& Parameters)
{
	FProviderFixture Fixture;
	Fixture.Provider.Init(true);
	Fixture.Provider.WaitForCommands(30.0);

	// Someone else holds Hero: refused, and the message names them.
	TSharedRef<FCheckOut> Refused = ISourceControlOperation::Create<FCheckOut>();
	TestEqual(TEXT("locked elsewhere fails"), Fixture.Run(Refused, { Fixture.File(TEXT("Content/Hero/Hero.uasset")) }), ECommandResult::Failed);
	TestTrue(TEXT("still locked by bob"), Fixture.State(TEXT("Content/Hero/Hero.uasset"))->IsCheckedOutOther());

	TSharedRef<FCheckOut> CheckOut = ISourceControlOperation::Create<FCheckOut>();
	TestEqual(TEXT("free file locks"), Fixture.Run(CheckOut, { Fixture.File(TEXT("Content/Maps/Main.umap")) }), ECommandResult::Succeeded);
	const FLodeStateRef Main = Fixture.State(TEXT("Content/Maps/Main.umap"));
	TestTrue(TEXT("checked out"), Main->IsCheckedOut());
	TestTrue(TEXT("editable"), Main->CanEdit());
	TestTrue(TEXT("lock sent"), Fixture.Calls().Contains(TEXT("lock {root}/Content/Maps/Main.umap")));

	// Revert of an unchanged locked file releases the lock: lode revert does not do that itself.
	TSharedRef<FRevert> Revert = ISourceControlOperation::Create<FRevert>();
	TestEqual(TEXT("revert succeeds"), Fixture.Run(Revert, { Fixture.File(TEXT("Content/Maps/Main.umap")) }), ECommandResult::Succeeded);
	TestTrue(TEXT("unlock sent"), Fixture.Calls().ContainsByPredicate([](const FString& Call) { return Call.StartsWith(TEXT("unlock")) && Call.EndsWith(TEXT("Main.umap")); }));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeProviderCheckInTest, "Lode.Provider.CheckIn", LODE_TEST_FLAGS)
bool FLodeProviderCheckInTest::RunTest(const FString& Parameters)
{
	FProviderFixture Fixture;
	Fixture.Provider.Init(true);
	Fixture.Provider.WaitForCommands(30.0);

	TSharedRef<FCheckIn> CheckIn = ISourceControlOperation::Create<FCheckIn>();
	const FString Message = TEXT("Brighter sky\n\nWith \"quotes\" and Ünïcödé");
	CheckIn->SetDescription(FText::FromString(Message));
	TestEqual(TEXT("commit succeeds"), Fixture.Run(CheckIn, { Fixture.File(TEXT("Config/DefaultGame.ini")) }), ECommandResult::Succeeded);
	TestTrue(TEXT("names the revision"), CheckIn->GetSuccessMessage().ToString().Contains(TEXT("5")));

	// The message goes through a file, never the command line.
	const TArray<FString> Calls = Fixture.Calls();
	TestTrue(TEXT("commit -F"), Calls.ContainsByPredicate([](const FString& Call) { return Call.StartsWith(TEXT("commit -F ")); }));
	TestFalse(TEXT("no message on the command line"), Calls.ContainsByPredicate([](const FString& Call) { return Call.Contains(TEXT("Brighter")); }));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeProviderOutsideTest, "Lode.Provider.OutsideWorkspace", LODE_TEST_FLAGS)
bool FLodeProviderOutsideTest::RunTest(const FString& Parameters)
{
	FProviderFixture Fixture;
	Fixture.Provider.Init(true);
	Fixture.Provider.WaitForCommands(30.0);

	// Engine content lives outside the workspace; lode would refuse the whole call for it.
	const FString Outside = LodePaths::Normalize(FPaths::ConvertRelativePathToFull(FPaths::EngineContentDir() / TEXT("Engine.uasset")));
	TSharedRef<FUpdateStatus> Update = ISourceControlOperation::Create<FUpdateStatus>();
	TestEqual(TEXT("status succeeds"), Fixture.Run(Update, { Outside, Fixture.File(TEXT("Content/Maps/Main.umap")) }), ECommandResult::Succeeded);
	TestFalse(TEXT("outside is not controlled"), Fixture.Provider.GetStateInternal(Outside)->IsSourceControlled());
	TestFalse(TEXT("outside cannot be added"), Fixture.Provider.GetStateInternal(Outside)->CanAdd());
	TestFalse(TEXT("outside path never reaches lode"), Fixture.Calls().ContainsByPredicate([&](const FString& Call) { return Call.Contains(TEXT("Engine.uasset")); }));
	return true;
}

#endif
