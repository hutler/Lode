// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeSourceControlModule.h"

#include "Features/IModularFeatures.h"
#include "Modules/ModuleManager.h"

#include "LodeOperations.h"

static const FName LodeModuleName(TEXT("LodeSourceControl"));
static const FName SourceControlFeatureName(TEXT("SourceControl"));

void FLodeSourceControlModule::StartupModule()
{
	LodeWorkers::RegisterAll(Provider);
	IModularFeatures::Get().RegisterModularFeature(SourceControlFeatureName, &Provider);
	EditorUI.Register();
}

void FLodeSourceControlModule::ShutdownModule()
{
	EditorUI.Unregister();
	Provider.Close();
	IModularFeatures::Get().UnregisterModularFeature(SourceControlFeatureName, &Provider);
}

FLodeSourceControlModule& FLodeSourceControlModule::Get()
{
	return FModuleManager::GetModuleChecked<FLodeSourceControlModule>(LodeModuleName);
}

FLodeSourceControlModule* FLodeSourceControlModule::GetIfLoaded()
{
	return FModuleManager::GetModulePtr<FLodeSourceControlModule>(LodeModuleName);
}

IMPLEMENT_MODULE(FLodeSourceControlModule, LodeSourceControl);
