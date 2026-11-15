// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"

/**
 * What the plugin adds to the editor besides the provider: the Lode Shelves window (Tools menu)
 * and the Lode section of the content browser's context menu (resolve a conflict to either side).
 */
class FLodeEditorUI
{
public:
	void Register();
	void Unregister();

private:
	void RegisterMenus();

	FDelegateHandle StartupCallbackHandle;
	bool bTabRegistered = false;
};
