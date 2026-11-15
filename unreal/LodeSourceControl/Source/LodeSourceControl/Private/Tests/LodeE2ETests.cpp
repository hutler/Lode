// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#if WITH_DEV_AUTOMATION_TESTS

#include "LodeTestHelpers.h"

#include "HAL/PlatformMisc.h"
#include "Misc/ScopeExit.h"
#include "SourceControlOperations.h"

#include "LodeOperations.h"
#include "LodeParsers.h"
#include "LodePaths.h"
#include "LodeProvider.h"

/**
 * unreal T10: the plugin against a real Lode server. Scripts/RunTests starts the server with
 * Tests/E2E/e2e_server.py and names its details file in LODE_E2E; without it these tests only
 * say they were skipped. Alice works through the plugin, Bob through plain `lode` calls. Users
 * are switched with LODE_CONFIG_DIR, so the tester's own sign-in is never touched.
 */

namespace
{
	struct FE2EUser
	{
		FString Root;
		FString Config;
	};

	struct FE2E
	{
		FString Lode;
		FE2EUser Alice;
		FE2EUser Bob;
		FString PreviousConfigDir;
		FLodeProvider Provider;

		bool Load()
		{
			const FString File = FPlatformMisc::GetEnvironmentVariable(TEXT("LODE_E2E"));
			FString Text;
			TSharedPtr<FJsonObject> Json;
			if (File.IsEmpty() || !FFileHelper::LoadFileToString(Text, *File)
				|| !FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Text), Json) || !Json.IsValid())
			{
				return false;
			}
			Lode = Json->GetStringField(TEXT("lode"));
			for (TPair<const TCHAR*, FE2EUser*> User : { TPair<const TCHAR*, FE2EUser*>(TEXT("alice"), &Alice), TPair<const TCHAR*, FE2EUser*>(TEXT("bob"), &Bob) })
			{
				const TSharedPtr<FJsonObject> Object = Json->GetObjectField(User.Key);
				User.Value->Root = LodePaths::Normalize(Object->GetStringField(TEXT("root")));
				User.Value->Config = Object->GetStringField(TEXT("config"));
			}
			PreviousConfigDir = FPlatformMisc::GetEnvironmentVariable(TEXT("LODE_CONFIG_DIR"));
			FPlatformMisc::SetEnvironmentVar(TEXT("LODE_CONFIG_DIR"), *Alice.Config);

			FLodeRunnerConfig Config;
			Config.Executable = Lode;
			Config.WorkingDirectory = Alice.Root;
			Provider.SetRunnerOverride(Config);
			Provider.SetWorkspaceRootOverride(Alice.Root);
			LodeWorkers::RegisterAll(Provider);
			Provider.Init(true);
			Provider.WaitForCommands(60.0);
			return true;
		}

		~FE2E()
		{
			Provider.Close();
			FPlatformMisc::SetEnvironmentVar(TEXT("LODE_CONFIG_DIR"), *PreviousConfigDir);
		}

		FString AliceFile(const TCHAR* Relative) const { return LodePaths::Normalize(Alice.Root / Relative); }
		FString BobFile(const TCHAR* Relative) const { return LodePaths::Normalize(Bob.Root / Relative); }

		ECommandResult::Type Run(const FSourceControlOperationRef& Operation, const TArray<FString>& Files, FSourceControlChangelistPtr Changelist = nullptr)
		{
			ECommandResult::Type Result = ECommandResult::Failed;
			bool bDone = false;
			Provider.Execute(Operation, Changelist, Files, EConcurrency::Asynchronous,
				FSourceControlOperationComplete::CreateLambda([&](const FSourceControlOperationRef&, ECommandResult::Type InResult)
				{
					Result = InResult;
					bDone = true;
				}));
			Provider.WaitForCommands(60.0);
			return bDone ? Result : ECommandResult::Failed;
		}

		/** One `lode` call as Bob, in Bob's workspace. */
		FLodeResult AsBob(const FLodeInvocation& Invocation)
		{
			FLodeRunnerConfig Config;
			Config.Executable = Lode;
			Config.WorkingDirectory = Bob.Root;
			Config.TempDirectory = Bob.Root / TEXT("..") / TEXT("tmp");
			FPlatformMisc::SetEnvironmentVar(TEXT("LODE_CONFIG_DIR"), *Bob.Config);
			ON_SCOPE_EXIT { FPlatformMisc::SetEnvironmentVar(TEXT("LODE_CONFIG_DIR"), *Alice.Config); };
			const FLodeRunner Runner(Config);
			return LodeTest::OffGameThread([&Runner, Invocation]() { return Runner.Run(Invocation); });
		}

		FLodeStateRef State(const TCHAR* Relative) { return Provider.GetStateInternal(AliceFile(Relative)); }

		void Refresh(const TArray<FString>& Files)
		{
			Run(ISourceControlOperation::Create<FUpdateStatus>(), Files);
			Run(ISourceControlOperation::Create<FLodeRefreshServer>(), Files);
		}
	};

	void AppendBytes(const FString& Filename, const FString& Text)
	{
		FString Existing;
		FFileHelper::LoadFileToString(Existing, *Filename);
		FFileHelper::SaveStringToFile(Existing + Text, *Filename);
	}
}

#define LODE_E2E_START(Fixture) \
	FE2E Fixture; \
	if (!Fixture.Load()) \
	{ \
		AddInfo(TEXT("Skipped: LODE_E2E is not set. Run Scripts/RunTests with -E2E to start a Lode server.")); \
		return true; \
	}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeE2EFlowTest, "Lode.E2E.Flow", LODE_TEST_FLAGS)
bool FLodeE2EFlowTest::RunTest(const FString& Parameters)
{
	LODE_E2E_START(E2E)

	// Connected as Alice.
	TestTrue(TEXT("connected"), E2E.Provider.IsAvailable());
	TestEqual(TEXT("signed in as alice"), E2E.Provider.GetWorkspace().UserName, TEXT("alice"));
	TestEqual(TEXT("depot"), E2E.Provider.GetWorkspace().Depot, TEXT("game"));

	const FString Hero = E2E.AliceFile(TEXT("Content/Hero/Hero.uasset"));
	const FString Main = E2E.AliceFile(TEXT("Content/Maps/Main.umap"));
	const FString Ini = E2E.AliceFile(TEXT("Config/DefaultEngine.ini"));
	const FString New = E2E.AliceFile(TEXT("Content/New/New.uasset"));

	// Bob locks Hero: Alice sees who has it and cannot check it out.
	TestTrue(TEXT("bob locks Hero"), E2E.AsBob({ TEXT("lock"), TEXT("Content/Hero/Hero.uasset"), TEXT("-m"), TEXT("lighting") }).IsOk());
	E2E.Refresh({ Hero, Main });
	FString Who;
	TestTrue(TEXT("Hero checked out by another"), E2E.State(TEXT("Content/Hero/Hero.uasset"))->IsCheckedOutOther(&Who));
	TestEqual(TEXT("by bob"), Who, TEXT("bob"));
	TestEqual(TEXT("check out refused"), E2E.Run(ISourceControlOperation::Create<FCheckOut>(), { Hero }), ECommandResult::Failed);

	// Alice locks the map, edits it, adds a new asset and submits both.
	TestTrue(TEXT("Main can be checked out"), E2E.State(TEXT("Content/Maps/Main.umap"))->CanCheckout());
	TestEqual(TEXT("check out Main"), E2E.Run(ISourceControlOperation::Create<FCheckOut>(), { Main }), ECommandResult::Succeeded);
	TestTrue(TEXT("Main checked out"), E2E.State(TEXT("Content/Maps/Main.umap"))->IsCheckedOut());
	AppendBytes(Main, TEXT("edit"));
	FFileHelper::SaveStringToFile(TEXT("new asset"), *New);
	TestEqual(TEXT("mark for add"), E2E.Run(ISourceControlOperation::Create<FMarkForAdd>(), { New }), ECommandResult::Succeeded);
	TestTrue(TEXT("New added"), E2E.State(TEXT("Content/New/New.uasset"))->IsAdded());

	TSharedRef<FCheckIn> CheckIn = ISourceControlOperation::Create<FCheckIn>();
	CheckIn->SetDescription(FText::FromString(TEXT("Brighter sky\n\nWith \"quotes\" & Ünïcödé")));
	TestEqual(TEXT("submit"), E2E.Run(CheckIn, { Main, New }), ECommandResult::Succeeded);
	E2E.Refresh({ Main, New });
	TestFalse(TEXT("Main no longer checked out (commit releases the lock)"), E2E.State(TEXT("Content/Maps/Main.umap"))->IsCheckedOut());
	TestTrue(TEXT("New is controlled"), E2E.State(TEXT("Content/New/New.uasset"))->IsSourceControlled());

	// History shows the submit with its message.
	TSharedRef<FUpdateStatus> History = ISourceControlOperation::Create<FUpdateStatus>();
	History->SetUpdateHistory(true);
	E2E.Run(History, { Main });
	const TSharedPtr<ISourceControlRevision, ESPMode::ThreadSafe> Newest = E2E.State(TEXT("Content/Maps/Main.umap"))->GetHistoryItem(0);
	if (TestTrue(TEXT("history"), Newest.IsValid()))
	{
		TestTrue(TEXT("message kept"), Newest->GetDescription().Contains(TEXT("Ünïcödé")));
		TestEqual(TEXT("author"), Newest->GetUserName(), TEXT("alice"));
	}

	// Bob changes the same line of a text file; Alice syncs into a conflict and keeps hers.
	TestTrue(TEXT("bob syncs"), E2E.AsBob({ TEXT("update") }).IsOk());
	FFileHelper::SaveStringToFile(TEXT("[/Script/Engine.RendererSettings]\nr.DefaultFeature.Bloom=False\n"), *E2E.BobFile(TEXT("Config/DefaultEngine.ini")));
	const FString BobMessage = E2E.Bob.Root / TEXT("..") / TEXT("bob-message.txt");
	FFileHelper::SaveStringToFile(TEXT("No bloom"), *BobMessage);
	TestTrue(TEXT("bob submits"), E2E.AsBob({ TEXT("commit"), TEXT("-F"), BobMessage, TEXT("Config/DefaultEngine.ini") }).IsOk());
	FFileHelper::SaveStringToFile(TEXT("[/Script/Engine.RendererSettings]\nr.DefaultFeature.Bloom=Maybe\n"), *Ini);
	E2E.Run(ISourceControlOperation::Create<FSync>(), { E2E.Alice.Root });
	E2E.Refresh({ Ini });
	TestTrue(TEXT("conflicted"), E2E.State(TEXT("Config/DefaultEngine.ini"))->IsConflicted());
	TSharedRef<FLodeResolve> Resolve = ISourceControlOperation::Create<FLodeResolve>();
	Resolve->bTakeTheirs = false;
	TestEqual(TEXT("resolve mine"), E2E.Run(Resolve, { Ini }), ECommandResult::Succeeded);
	TestFalse(TEXT("no longer conflicted"), E2E.State(TEXT("Config/DefaultEngine.ini"))->IsConflicted());
	FString IniText;
	FFileHelper::LoadFileToString(IniText, *Ini);
	TestTrue(TEXT("kept mine"), IniText.Contains(TEXT("Bloom=Maybe")) && !IniText.Contains(TEXT("<<<<<<<")));

	// Alice shelves the change; Bob unshelves it.
	TSharedRef<FShelve> Shelve = ISourceControlOperation::Create<FShelve>();
	Shelve->SetDescription(FText::FromString(TEXT("Bloom tweak")));
	TestEqual(TEXT("shelve"), E2E.Run(Shelve, { Ini }), ECommandResult::Succeeded);
	int32 ShelfId = E2E.Provider.GetShelfForChangelist(TEXT("Bloom-tweak"));
	TestTrue(TEXT("shelf remembered for the changelist"), ShelfId > 0);
	TSharedRef<FLodeListShelves> List = ISourceControlOperation::Create<FLodeListShelves>();
	TestEqual(TEXT("list shelves"), E2E.Run(List, {}), ECommandResult::Succeeded);
	TestTrue(TEXT("shelf listed"), List->Shelves.ContainsByPredicate([ShelfId](const FLodeShelfRecord& Shelf) { return Shelf.Id == ShelfId; }));
	TestTrue(TEXT("bob syncs again"), E2E.AsBob({ TEXT("update") }).IsOk());
	TestTrue(TEXT("bob unshelves"), E2E.AsBob({ TEXT("unshelve"), FString::FromInt(ShelfId) }).IsOk());
	FString BobIni;
	FFileHelper::LoadFileToString(BobIni, *E2E.BobFile(TEXT("Config/DefaultEngine.ini")));
	TestTrue(TEXT("bob has alice's change"), BobIni.Contains(TEXT("Bloom=Maybe")));

	// Revert of an untouched checkout releases the lock.
	TestEqual(TEXT("check out again"), E2E.Run(ISourceControlOperation::Create<FCheckOut>(), { Main }), ECommandResult::Succeeded);
	TestEqual(TEXT("revert"), E2E.Run(ISourceControlOperation::Create<FRevert>(), { Main }), ECommandResult::Succeeded);
	const FLodeResult Locks = E2E.AsBob({ TEXT("locks") });
	TestTrue(TEXT("alice holds no lock on Main"), Locks.IsOk() && !LodeParsers::ParseLocks(*Locks.Json, E2E.Bob.Root).ContainsByPredicate(
		[](const FLodeLockRecord& Lock) { return Lock.DepotPath == TEXT("Content/Maps/Main.umap"); }));

	// Leave the server as it was for the next run: Bob lets go of Hero.
	E2E.AsBob({ TEXT("unlock"), TEXT("Content/Hero/Hero.uasset") });
	return true;
}

#undef LODE_E2E_START

#endif
