// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#if WITH_DEV_AUTOMATION_TESTS

#include "LodeTestHelpers.h"

#include "Misc/ScopeExit.h"

#include "LodeParsers.h"
#include "LodePaths.h"

/**
 * unreal T03/T04/T07/T08: the parsers against answers recorded from a real lode 0.2.0
 * (Tests/FakeLode/scenarios/workspace.json, made by record_scenarios.py).
 */

namespace
{
	const FLodeFileRecord* FindRecord(const TArray<FLodeFileRecord>& Records, const FString& DepotPath)
	{
		return Records.FindByPredicate([&](const FLodeFileRecord& Record) { return Record.DepotPath == DepotPath; });
	}
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserStatusTest, "Lode.Parsers.Status", LODE_TEST_FLAGS)
bool FLodeParserStatusTest::RunTest(const FString& Parameters)
{
	const FString Root = LodeTest::MakeTempDir(TEXT("status"));
	ON_SCOPE_EXIT { LodeTest::RemoveDir(Root); };
	// An ignored file is on disk but lode does not list it.
	FFileHelper::SaveStringToFile(TEXT("log"), *(Root / TEXT("Saved/x.log")));

	const TSharedPtr<FJsonObject> Json = LodeTest::Answer(TEXT("workspace"), TEXT("status --unchanged --remote *"), Root);
	if (!TestTrue(TEXT("recorded answer found"), Json.IsValid()))
	{
		return false;
	}

	TArray<FLodeFileRecord> Records;
	FString Branch;
	int32 Rev = 0;
	const TArray<FString> Requested = { Root / TEXT("Saved/x.log"), Root / TEXT("Content/New.uasset"), Root / TEXT("Content") };
	LodeParsers::ParseStatus(*Json, Root, Requested, Records, Branch, Rev);

	TestEqual(TEXT("branch"), Branch, TEXT("main"));
	TestEqual(TEXT("rev"), Rev, 2);

	const FLodeFileRecord* Hero = FindRecord(Records, TEXT("Content/Hero/Hero.uasset"));
	if (TestNotNull(TEXT("Hero listed"), Hero))
	{
		TestTrue(TEXT("Hero unchanged"), Hero->Status == ELodeFileStatus::Unchanged);
		TestTrue(TEXT("Hero needs locks"), Hero->bLockRequired);
		TestFalse(TEXT("Hero not locked here"), Hero->bLockedHere);
		TestTrue(TEXT("remote known"), Hero->bRemoteKnown);
		TestEqual(TEXT("locked by bob"), Hero->LockOwner, TEXT("bob"));
		TestEqual(TEXT("head rev"), Hero->HeadRev, 2);
		TestEqual(TEXT("filename"), Hero->Filename, LodePaths::Normalize(Root / TEXT("Content/Hero/Hero.uasset")));
	}

	const FLodeFileRecord* Project = FindRecord(Records, TEXT("Game.uproject"));
	if (TestNotNull(TEXT("project listed"), Project))
	{
		TestFalse(TEXT("project needs no lock"), Project->bLockRequired);
		TestTrue(TEXT("nobody holds it"), Project->LockOwner.IsEmpty());
		TestTrue(TEXT("remote known for files the server did not name"), Project->bRemoteKnown);
	}

	const FLodeFileRecord* Log = FindRecord(Records, TEXT("Saved/x.log"));
	if (TestNotNull(TEXT("ignored file gets a record"), Log))
	{
		TestTrue(TEXT("on disk, not listed: ignored"), Log->Status == ELodeFileStatus::Ignored);
	}
	const FLodeFileRecord* New = FindRecord(Records, TEXT("Content/New.uasset"));
	if (TestNotNull(TEXT("missing file gets a record"), New))
	{
		TestTrue(TEXT("not on disk, not listed: untracked"), New->Status == ELodeFileStatus::Untracked);
	}
	TestNull(TEXT("folders get no record"), FindRecord(Records, TEXT("Content")));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserLocksTest, "Lode.Parsers.Locks", LODE_TEST_FLAGS)
bool FLodeParserLocksTest::RunTest(const FString& Parameters)
{
	const FString Root = TEXT("C:/Work/Game");
	const TSharedPtr<FJsonObject> Json = LodeTest::Answer(TEXT("workspace"), TEXT("locks"), Root);
	if (!TestTrue(TEXT("recorded answer found"), Json.IsValid()))
	{
		return false;
	}
	const TArray<FLodeLockRecord> Locks = LodeParsers::ParseLocks(*Json, Root);
	if (TestEqual(TEXT("one lock"), Locks.Num(), 1))
	{
		TestEqual(TEXT("path"), Locks[0].DepotPath, TEXT("Content/Maps/Main.umap"));
		TestEqual(TEXT("owner"), Locks[0].Owner, TEXT("alice"));
		TestTrue(TEXT("mine"), Locks[0].bMine);
		TestEqual(TEXT("year"), Locks[0].CreatedAt.GetYear(), 2026);
	}

	// The error lode prints when someone else holds the lock names them in details.locks.
	const TSharedPtr<FJsonObject> Refused = LodeTest::Answer(TEXT("workspace"), TEXT("lock Content/Hero/Hero.uasset"), Root);
	if (TestTrue(TEXT("refusal recorded"), Refused.IsValid()))
	{
		const TSharedPtr<FJsonObject> Details = Refused->GetObjectField(TEXT("error"))->GetObjectField(TEXT("details"));
		const TArray<FLodeLockRecord> Held = LodeParsers::ParseLocks(*Details, Root);
		if (TestEqual(TEXT("one holder"), Held.Num(), 1))
		{
			TestEqual(TEXT("bob holds it"), Held[0].Owner, TEXT("bob"));
			TestEqual(TEXT("comment"), Held[0].Comment, TEXT("lighting"));
		}
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserLogTest, "Lode.Parsers.FileLog", LODE_TEST_FLAGS)
bool FLodeParserLogTest::RunTest(const FString& Parameters)
{
	const TSharedPtr<FJsonObject> Json = LodeTest::Answer(TEXT("workspace"), TEXT("log ? -v --follow -n ?"), TEXT("C:/Work/Game"));
	if (!TestTrue(TEXT("recorded answer found"), Json.IsValid()))
	{
		return false;
	}
	const TArray<FLodeRevisionRecord> History = LodeParsers::ParseFileLog(*Json, TEXT("Content/Maps/Main.umap"));
	if (TestTrue(TEXT("at least one revision"), History.Num() >= 1))
	{
		TestEqual(TEXT("newest first"), History[0].Rev, 3);
		TestEqual(TEXT("author"), History[0].Author, TEXT("alice"));
		TestEqual(TEXT("action"), History[0].Action, TEXT("modify"));
		TestEqual(TEXT("size"), History[0].Size, int64(2004));
		TestTrue(TEXT("message"), History[0].Message.StartsWith(TEXT("Brighter sky")));
	}
	// Other files in the same revisions are left out.
	TestEqual(TEXT("unrelated path gives nothing"), LodeParsers::ParseFileLog(*Json, TEXT("Other.txt")).Num(), 0);

	// A hand-made rename: history follows copy_from back to the old name.
	const FString Renamed = TEXT(R"({"ok":true,"revisions":[
		{"rev":5,"author":{"name":"bob","kind":"user"},"submitter":{"name":"ci","kind":"agent"},"message":"Rename","created_at":"2026-01-02T03:04:05Z",
		 "changes":[{"action":"add","path":"New.uasset","copy_from":{"path":"Old.uasset","rev":4}},{"action":"delete","path":"Old.uasset"}]},
		{"rev":4,"author":{"name":"alice","kind":"user"},"message":"Edit","created_at":"2026-01-01T00:00:00Z",
		 "changes":[{"action":"modify","path":"Old.uasset","size":10}]}]})");
	TSharedPtr<FJsonObject> RenameJson;
	FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Renamed), RenameJson);
	const TArray<FLodeRevisionRecord> Followed = LodeParsers::ParseFileLog(*RenameJson, TEXT("New.uasset"));
	if (TestEqual(TEXT("two revisions"), Followed.Num(), 2))
	{
		TestEqual(TEXT("rename source"), Followed[0].CopyFromPath, TEXT("Old.uasset"));
		TestEqual(TEXT("submitter kept"), Followed[0].Submitter, TEXT("ci"));
		TestEqual(TEXT("older under the old name"), Followed[1].DepotPath, TEXT("Old.uasset"));
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserChangelistsTest, "Lode.Parsers.Changelists", LODE_TEST_FLAGS)
bool FLodeParserChangelistsTest::RunTest(const FString& Parameters)
{
	const FString Root = TEXT("C:/Work/Game");
	const TSharedPtr<FJsonObject> Json = LodeTest::Answer(TEXT("workspace"), TEXT("changelist list"), Root);
	if (!TestTrue(TEXT("recorded answer found"), Json.IsValid()))
	{
		return false;
	}
	const TArray<FLodeChangelistRecord> Lists = LodeParsers::ParseChangelists(*Json, Root);
	const FLodeChangelistRecord* Art = Lists.FindByPredicate([](const FLodeChangelistRecord& List) { return List.Name == TEXT("Art"); });
	if (TestNotNull(TEXT("Art listed"), Art))
	{
		TestEqual(TEXT("description"), Art->Description, TEXT("Art pass"));
		if (TestEqual(TEXT("one file"), Art->Filenames.Num(), 1))
		{
			TestEqual(TEXT("absolute filename"), Art->Filenames[0], LodePaths::Normalize(Root / TEXT("Config/DefaultEngine.ini")));
		}
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserShelvesTest, "Lode.Parsers.Shelves", LODE_TEST_FLAGS)
bool FLodeParserShelvesTest::RunTest(const FString& Parameters)
{
	const FString Root = TEXT("C:/Work/Game");
	const TSharedPtr<FJsonObject> List = LodeTest::Answer(TEXT("workspace"), TEXT("shelves --state open"), Root);
	const TSharedPtr<FJsonObject> Show = LodeTest::Answer(TEXT("workspace"), TEXT("shelf show ?"), Root);
	if (!TestTrue(TEXT("recorded answers found"), List.IsValid() && Show.IsValid()))
	{
		return false;
	}
	const TArray<FLodeShelfRecord> Shelves = LodeParsers::ParseShelves(*List);
	if (TestEqual(TEXT("one shelf"), Shelves.Num(), 1))
	{
		TestEqual(TEXT("id"), Shelves[0].Id, 1);
		TestEqual(TEXT("owner"), Shelves[0].Owner, TEXT("alice"));
		TestEqual(TEXT("state"), Shelves[0].State, TEXT("open"));
		TestEqual(TEXT("base"), Shelves[0].BaseRev, 3);
	}
	const FLodeShelfRecord Shelf = LodeParsers::ParseShelf(*Show, Root);
	TestEqual(TEXT("description"), Shelf.Description, TEXT("Art pass"));
	if (TestEqual(TEXT("one change"), Shelf.Filenames.Num(), 1))
	{
		TestEqual(TEXT("file"), Shelf.Filenames[0], LodePaths::Normalize(Root / TEXT("Config/DefaultEngine.ini")));
		TestEqual(TEXT("action"), Shelf.Actions[0], TEXT("modify"));
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserConflictsTest, "Lode.Parsers.Conflicts", LODE_TEST_FLAGS)
bool FLodeParserConflictsTest::RunTest(const FString& Parameters)
{
	const FString Root = TEXT("C:/Work/Game");
	const TSharedPtr<FJsonObject> Update = LodeTest::Answer(TEXT("workspace"), TEXT("update"), Root);
	if (TestTrue(TEXT("recorded answer found"), Update.IsValid()))
	{
		TestEqual(TEXT("a clean update has no conflicts"), LodeParsers::ParseConflicts(*Update, Root).Num(), 0);
	}
	TSharedPtr<FJsonObject> Json;
	FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(FString(TEXT(
		R"({"ok":true,"files":[{"path":"A.uasset","outcome":"conflicted"},{"path":"B.txt","outcome":"merged"}]})"))), Json);
	const TArray<FString> Conflicts = LodeParsers::ParseConflicts(*Json, Root);
	if (TestEqual(TEXT("one conflict"), Conflicts.Num(), 1))
	{
		TestEqual(TEXT("file"), Conflicts[0], LodePaths::Normalize(Root / TEXT("A.uasset")));
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeParserTimeTest, "Lode.Parsers.Time", LODE_TEST_FLAGS)
bool FLodeParserTimeTest::RunTest(const FString& Parameters)
{
	const FDateTime Time = LodeParsers::ParseTime(TEXT("2026-11-14T22:59:36.042Z"));
	TestEqual(TEXT("year"), Time.GetYear(), 2026);
	TestEqual(TEXT("month"), Time.GetMonth(), 11);
	TestEqual(TEXT("hour"), Time.GetHour(), 22);
	TestTrue(TEXT("bad text gives zero"), LodeParsers::ParseTime(TEXT("yesterday")) == FDateTime(0));
	TestTrue(TEXT("status words"), LodeParsers::ParseStatusValue(TEXT("conflicted")) == ELodeFileStatus::Conflicted);
	TestTrue(TEXT("unknown status word"), LodeParsers::ParseStatusValue(TEXT("sideways")) == ELodeFileStatus::Unknown);
	return true;
}

#endif
