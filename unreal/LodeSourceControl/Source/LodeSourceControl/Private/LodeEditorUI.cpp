// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeEditorUI.h"

#include "AssetRegistry/AssetData.h"
#include "ContentBrowserMenuContexts.h"
#include "Framework/Docking/TabManager.h"
#include "ISourceControlModule.h"
#include "ISourceControlProvider.h"
#include "Misc/PackageName.h"
#include "SourceControlOperations.h"
#include "ToolMenus.h"
#include "Widgets/Docking/SDockTab.h"

#include "LodeOperations.h"
#include "LodeProvider.h"
#include "LodeSourceControlModule.h"
#include "SLodeShelves.h"

#define LOCTEXT_NAMESPACE "LodeSourceControl.UI"

namespace
{
	const FName ShelvesTabName(TEXT("LodeShelves"));
	const FName MenuOwner(TEXT("LodeSourceControl"));

	bool IsLodeActive()
	{
		return ISourceControlModule::Get().IsEnabled() && ISourceControlModule::Get().GetProvider().GetName() == FName(TEXT("Lode"));
	}

	TArray<FString> FilenamesOf(const TArray<FAssetData>& Assets)
	{
		TArray<FString> Files;
		for (const FAssetData& Asset : Assets)
		{
			FString Filename;
			if (FPackageName::DoesPackageExist(Asset.PackageName.ToString(), &Filename))
			{
				Files.Add(FPaths::ConvertRelativePathToFull(Filename));
			}
		}
		return Files;
	}

	TArray<FString> ConflictedOf(const TArray<FString>& Files)
	{
		TArray<FString> Conflicted;
		TArray<FSourceControlStateRef> States;
		if (ISourceControlModule::Get().GetProvider().GetState(Files, States, EStateCacheUsage::Use) == ECommandResult::Succeeded)
		{
			for (const FSourceControlStateRef& State : States)
			{
				if (State->IsConflicted())
				{
					Conflicted.Add(State->GetFilename());
				}
			}
		}
		return Conflicted;
	}

	void Resolve(const TArray<FString> Files, bool bTheirs)
	{
		TSharedRef<FLodeResolve, ESPMode::ThreadSafe> Operation = ISourceControlOperation::Create<FLodeResolve>();
		Operation->bTakeTheirs = bTheirs;
		ISourceControlModule::Get().GetProvider().Execute(Operation, Files, EConcurrency::Synchronous);
	}
}

void FLodeEditorUI::Register()
{
	FGlobalTabmanager::Get()->RegisterNomadTabSpawner(ShelvesTabName, FOnSpawnTab::CreateLambda([](const FSpawnTabArgs&)
	{
		return SNew(SDockTab)
			.TabRole(ETabRole::NomadTab)
			[
				SNew(SLodeShelves)
			];
	}))
	.SetDisplayName(LOCTEXT("ShelvesTab", "Lode Shelves"))
	.SetTooltipText(LOCTEXT("ShelvesTabTip", "Open shelves on the Lode server, everyone's: look at them and unshelve them here."))
	.SetMenuType(ETabSpawnerMenuType::Hidden);
	bTabRegistered = true;

	StartupCallbackHandle = UToolMenus::RegisterStartupCallback(FSimpleMulticastDelegate::FDelegate::CreateRaw(this, &FLodeEditorUI::RegisterMenus));
}

void FLodeEditorUI::Unregister()
{
	if (UObjectInitialized())
	{
		UToolMenus::UnRegisterStartupCallback(StartupCallbackHandle);
		UToolMenus::UnregisterOwner(MenuOwner);
	}
	if (bTabRegistered && FSlateApplication::IsInitialized())
	{
		FGlobalTabmanager::Get()->UnregisterNomadTabSpawner(ShelvesTabName);
	}
	bTabRegistered = false;
}

void FLodeEditorUI::RegisterMenus()
{
	FToolMenuOwnerScoped OwnerScoped(MenuOwner);

	// Tools > Lode Shelves
	if (UToolMenu* Tools = UToolMenus::Get()->ExtendMenu("LevelEditor.MainMenu.Tools"))
	{
		FToolMenuSection& Section = Tools->FindOrAddSection("Lode", LOCTEXT("LodeSection", "Lode"));
		Section.AddMenuEntry(
			"LodeShelves",
			LOCTEXT("OpenShelves", "Lode Shelves"),
			LOCTEXT("OpenShelvesTip", "Open shelves on the Lode server, everyone's: look at them and unshelve them here."),
			FSlateIcon(),
			FUIAction(
				FExecuteAction::CreateLambda([]() { FGlobalTabmanager::Get()->TryInvokeTab(ShelvesTabName); }),
				FCanExecuteAction::CreateLambda([]() { return IsLodeActive(); })));
	}

	// Content browser: resolve conflicted assets to either side.
	if (UToolMenu* AssetMenu = UToolMenus::Get()->ExtendMenu("ContentBrowser.AssetContextMenu"))
	{
		AssetMenu->AddDynamicSection("LodeResolve", FNewToolMenuDelegate::CreateLambda([](UToolMenu* InMenu)
		{
			if (!IsLodeActive())
			{
				return;
			}
			const UContentBrowserAssetContextMenuContext* Context = InMenu->FindContext<UContentBrowserAssetContextMenuContext>();
			if (!Context)
			{
				return;
			}
			const TArray<FString> Conflicted = ConflictedOf(FilenamesOf(Context->SelectedAssets));
			if (Conflicted.Num() == 0)
			{
				return;
			}
			FToolMenuSection& Section = InMenu->AddSection("Lode", LOCTEXT("LodeConflicts", "Lode conflict"));
			Section.AddMenuEntry(
				"LodeKeepMine",
				LOCTEXT("KeepMine", "Keep My Version"),
				LOCTEXT("KeepMineTip", "Settle the conflict with the asset as it is in your workspace. Your submit will replace the server's version."),
				FSlateIcon(),
				FUIAction(FExecuteAction::CreateLambda([Conflicted]() { Resolve(Conflicted, false); })));
			Section.AddMenuEntry(
				"LodeTakeTheirs",
				LOCTEXT("TakeTheirs", "Take Their Version"),
				LOCTEXT("TakeTheirsTip", "Settle the conflict with the server's version (or the shelf's). Your changes to this asset are lost."),
				FSlateIcon(),
				FUIAction(FExecuteAction::CreateLambda([Conflicted]() { Resolve(Conflicted, true); })));
		}));
	}
}

#undef LOCTEXT_NAMESPACE
