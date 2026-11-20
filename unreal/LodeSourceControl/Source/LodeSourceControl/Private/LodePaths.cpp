// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodePaths.h"

#include "Misc/Paths.h"

namespace
{
	ESearchCase::Type PlatformCase()
	{
#if PLATFORM_WINDOWS || PLATFORM_MAC
		return ESearchCase::IgnoreCase;
#else
		return ESearchCase::CaseSensitive;
#endif
	}
}

FString LodePaths::Normalize(const FString& Path)
{
	FString Out = FPaths::ConvertRelativePathToFull(Path);
	FPaths::NormalizeFilename(Out);
	FPaths::CollapseRelativeDirectories(Out);
	while (Out.Len() > 1 && Out.EndsWith(TEXT("/")) && !Out.EndsWith(TEXT(":/")))
	{
		Out.LeftChopInline(1);
	}
	return Out;
}

FString LodePaths::FindWorkspaceRoot(const FString& Start)
{
	FString Directory = Normalize(Start);
	for (int32 Depth = 0; Depth < 64 && !Directory.IsEmpty(); ++Depth)
	{
		if (FPaths::DirectoryExists(Directory / TEXT(".lode")))
		{
			return Directory;
		}
		const FString Parent = FPaths::GetPath(Directory);
		if (Parent.IsEmpty() || Parent == Directory)
		{
			break;
		}
		Directory = Parent;
	}
	return FString();
}

bool LodePaths::IsInWorkspace(const FString& WorkspaceRoot, const FString& Filename)
{
	if (WorkspaceRoot.IsEmpty())
	{
		return false;
	}
	const FString Full = Normalize(Filename);
	const FString Prefix = WorkspaceRoot.EndsWith(TEXT("/")) ? WorkspaceRoot : WorkspaceRoot + TEXT("/");
	return Full.StartsWith(Prefix, PlatformCase()) && Full.Len() > Prefix.Len();
}

FString LodePaths::ToDepotPath(const FString& WorkspaceRoot, const FString& Filename)
{
	if (!IsInWorkspace(WorkspaceRoot, Filename))
	{
		return FString();
	}
	const FString Full = Normalize(Filename);
	const int32 PrefixLength = WorkspaceRoot.EndsWith(TEXT("/")) ? WorkspaceRoot.Len() : WorkspaceRoot.Len() + 1;
	return Full.RightChop(PrefixLength);
}

FString LodePaths::ToFilename(const FString& WorkspaceRoot, const FString& DepotPath)
{
	return Normalize(WorkspaceRoot / DepotPath);
}
