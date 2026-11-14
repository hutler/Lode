// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "Modules/ModuleInterface.h"

#include "LodeEditorUI.h"
#include "LodeProvider.h"

/** The plugin's one module: owns the provider and registers it under the "SourceControl" feature name. */
class FLodeSourceControlModule : public IModuleInterface
{
public:
	virtual void StartupModule() override;
	virtual void ShutdownModule() override;

	FLodeProvider& GetProvider() { return Provider; }
	const FLodeProvider& GetProvider() const { return Provider; }

	static FLodeSourceControlModule& Get();
	static FLodeSourceControlModule* GetIfLoaded();

private:
	FLodeProvider Provider;
	FLodeEditorUI EditorUI;
};
