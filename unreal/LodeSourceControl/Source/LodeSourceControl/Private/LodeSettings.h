// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"
#include "HAL/CriticalSection.h"

/**
 * The plugin's settings, kept in the editor's revision control settings file
 * (Saved/Config/<Platform>Editor/SourceControlSettings.ini). Never holds a token or password.
 */
class FLodeSettings
{
public:
	/** Path to lode, or empty to look on PATH. */
	FString GetLodePath() const;
	void SetLodePath(const FString& InPath);

	/** Seconds between refreshes of server information (locks, newer revisions) for assets in view. */
	int32 GetServerRefreshSeconds() const;
	void SetServerRefreshSeconds(int32 InSeconds);

	/** Default for "keep locks" in the submit window. */
	bool GetKeepLocksDefault() const;
	void SetKeepLocksDefault(bool bInKeep);

	void Load();
	void Save() const;

private:
	mutable FCriticalSection Lock;
	FString LodePath;
	int32 ServerRefreshSeconds = 60;
	bool bKeepLocksDefault = false;
};
