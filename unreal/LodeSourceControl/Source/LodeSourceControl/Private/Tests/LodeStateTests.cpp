// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#if WITH_DEV_AUTOMATION_TESTS

#include "LodeTestHelpers.h"

#include "LodeOperations.h"
#include "LodeState.h"

/** unreal T04/T05: what the editor may do with a file, from its Lode record. */

namespace
{
	FLodeStateRef MakeState(TFunctionRef<void(FLodeFileRecord&)> Setup)
	{
		FLodeFileRecord Record;
		Record.Filename = TEXT("C:/Work/Game/Content/Hero.uasset");
		Record.DepotPath = TEXT("Content/Hero.uasset");
		Record.Status = ELodeFileStatus::Unchanged;
		Record.bRemoteKnown = true;
		Setup(Record);
		FLodeStateRef State = MakeShared<FLodeState, ESPMode::ThreadSafe>(Record.Filename);
		State->SetRecord(Record);
		return State;
	}
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeStateRulesTest, "Lode.State.Rules", LODE_TEST_FLAGS)
bool FLodeStateRulesTest::RunTest(const FString& Parameters)
{
	{
		const FLodeStateRef Free = MakeState([](FLodeFileRecord& R) { R.bLockRequired = true; });
		TestTrue(TEXT("lock-required file can be checked out"), Free->CanCheckout());
		TestFalse(TEXT("but not edited before"), Free->CanEdit());
		TestFalse(TEXT("nothing to check in"), Free->CanCheckIn());
		TestFalse(TEXT("nothing to revert"), Free->CanRevert());
	}
	{
		const FLodeStateRef Mine = MakeState([](FLodeFileRecord& R) { R.bLockRequired = true; R.bLockedHere = true; });
		TestTrue(TEXT("locked here is checked out"), Mine->IsCheckedOut());
		TestTrue(TEXT("locked here can be edited"), Mine->CanEdit());
		TestFalse(TEXT("no second checkout"), Mine->CanCheckout());
		TestTrue(TEXT("a lock alone can be reverted (released)"), Mine->CanRevert());
	}
	{
		const FLodeStateRef Theirs = MakeState([](FLodeFileRecord& R) { R.bLockRequired = true; R.LockOwner = TEXT("bob"); });
		FString Who;
		TestTrue(TEXT("checked out by someone else"), Theirs->IsCheckedOutOther(&Who));
		TestEqual(TEXT("names the holder"), Who, TEXT("bob"));
		TestFalse(TEXT("cannot check out"), Theirs->CanCheckout());
		TestFalse(TEXT("cannot edit"), Theirs->CanEdit());
		TestFalse(TEXT("cannot delete"), Theirs->CanDelete());
	}
	{
		const FLodeStateRef Text = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Modified; });
		TestFalse(TEXT("plain files need no checkout"), Text->CanCheckout());
		TestTrue(TEXT("plain files are editable"), Text->CanEdit());
		TestTrue(TEXT("modified can be checked in"), Text->CanCheckIn());
		TestTrue(TEXT("modified counts as checked out"), Text->IsCheckedOut());
	}
	{
		const FLodeStateRef NeedsLock = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Modified; R.bLockRequired = true; R.bNeedsLock = true; });
		TestFalse(TEXT("a change that needs a lock cannot be checked in"), NeedsLock->CanCheckIn());
	}
	{
		const FLodeStateRef Conflict = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Conflicted; });
		TestTrue(TEXT("conflicted"), Conflict->IsConflicted());
		TestFalse(TEXT("conflicts block check in"), Conflict->CanCheckIn());
	}
	{
		const FLodeStateRef Behind = MakeState([](FLodeFileRecord& R) { R.bNewerAtHead = true; R.HeadRev = 9; });
		TestFalse(TEXT("newer on the server"), Behind->IsCurrent());
	}
	{
		const FLodeStateRef New = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Untracked; });
		TestTrue(TEXT("untracked can be added"), New->CanAdd());
		TestFalse(TEXT("untracked is not controlled"), New->IsSourceControlled());
		const FLodeStateRef Outside = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Untracked; R.DepotPath.Reset(); });
		TestFalse(TEXT("outside the workspace cannot be added"), Outside->CanAdd());
		const FLodeStateRef Ignored = MakeState([](FLodeFileRecord& R) { R.Status = ELodeFileStatus::Ignored; });
		TestTrue(TEXT("ignored"), Ignored->IsIgnored());
		TestFalse(TEXT("ignored cannot be added"), Ignored->CanAdd());
	}
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeVersionTest, "Lode.State.Version", LODE_TEST_FLAGS)
bool FLodeVersionTest::RunTest(const FString& Parameters)
{
	TestTrue(TEXT("0.2.0"), LodeWorkers::IsVersionSupported(TEXT("0.2.0")));
	TestTrue(TEXT("0.2.1"), LodeWorkers::IsVersionSupported(TEXT("0.2.1")));
	TestTrue(TEXT("1.0"), LodeWorkers::IsVersionSupported(TEXT("1.0")));
	TestTrue(TEXT("0.3.0-dev"), LodeWorkers::IsVersionSupported(TEXT("0.3.0-dev")));
	TestFalse(TEXT("0.1.9"), LodeWorkers::IsVersionSupported(TEXT("0.1.9")));
	TestFalse(TEXT("garbage"), LodeWorkers::IsVersionSupported(TEXT("lode")));
	TestFalse(TEXT("empty"), LodeWorkers::IsVersionSupported(TEXT("")));
	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FLodeChangelistNameTest, "Lode.State.ChangelistName", LODE_TEST_FLAGS)
bool FLodeChangelistNameTest::RunTest(const FString& Parameters)
{
	TestEqual(TEXT("first line, dashes"), LodeWorkers::MakeChangelistName(TEXT("Fix hero lighting!\nMore text"), {}), TEXT("Fix-hero-lighting"));
	TestEqual(TEXT("empty gets a name"), LodeWorkers::MakeChangelistName(TEXT("  "), {}), TEXT("changes"));
	TestEqual(TEXT("never 'default'"), LodeWorkers::MakeChangelistName(TEXT("default"), {}), TEXT("changes"));
	TestEqual(TEXT("unique"), LodeWorkers::MakeChangelistName(TEXT("Art"), { TEXT("Art"), TEXT("Art-2") }), TEXT("Art-3"));
	TestTrue(TEXT("short"), LodeWorkers::MakeChangelistName(FString::ChrN(100, TEXT('a')), {}).Len() <= 32);
	return true;
}

#endif
