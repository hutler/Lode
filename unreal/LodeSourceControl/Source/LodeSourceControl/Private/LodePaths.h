// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#include "CoreMinimal.h"

/** Paths between the editor (absolute filenames) and Lode (depot paths relative to the workspace root). */
namespace LodePaths
{
	/** Absolute, forward slashes, no trailing slash. */
	FString Normalize(const FString& Path);

	/** Walks up from Start to the folder that holds `.lode`. Empty when there is none. */
	FString FindWorkspaceRoot(const FString& Start);

	/** The depot path of an absolute filename, or empty when it is outside the workspace (or is the root). */
	FString ToDepotPath(const FString& WorkspaceRoot, const FString& Filename);

	/** The absolute filename of a depot path. */
	FString ToFilename(const FString& WorkspaceRoot, const FString& DepotPath);

	/** True when Filename is inside the workspace root (case-insensitive on Windows). */
	bool IsInWorkspace(const FString& WorkspaceRoot, const FString& Filename);
}
