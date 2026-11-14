// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#include "LodeSettings.h"

#include "Misc/ConfigCacheIni.h"
#include "Misc/ScopeLock.h"
#include "SourceControlHelpers.h"

namespace
{
	const TCHAR* Section = TEXT("LodeSourceControl.LodeSourceControlSettings");
}

FString FLodeSettings::GetLodePath() const
{
	FScopeLock ScopeLock(&Lock);
	return LodePath;
}

void FLodeSettings::SetLodePath(const FString& InPath)
{
	FScopeLock ScopeLock(&Lock);
	LodePath = InPath.TrimStartAndEnd().TrimQuotes();
}

int32 FLodeSettings::GetServerRefreshSeconds() const
{
	FScopeLock ScopeLock(&Lock);
	return ServerRefreshSeconds;
}

void FLodeSettings::SetServerRefreshSeconds(int32 InSeconds)
{
	FScopeLock ScopeLock(&Lock);
	ServerRefreshSeconds = FMath::Clamp(InSeconds, 10, 3600);
}

bool FLodeSettings::GetKeepLocksDefault() const
{
	FScopeLock ScopeLock(&Lock);
	return bKeepLocksDefault;
}

void FLodeSettings::SetKeepLocksDefault(bool bInKeep)
{
	FScopeLock ScopeLock(&Lock);
	bKeepLocksDefault = bInKeep;
}

void FLodeSettings::Load()
{
	FScopeLock ScopeLock(&Lock);
	const FString& Ini = SourceControlHelpers::GetSettingsIni();
	GConfig->GetString(Section, TEXT("LodePath"), LodePath, Ini);
	GConfig->GetInt(Section, TEXT("ServerRefreshSeconds"), ServerRefreshSeconds, Ini);
	GConfig->GetBool(Section, TEXT("KeepLocksDefault"), bKeepLocksDefault, Ini);
	ServerRefreshSeconds = FMath::Clamp(ServerRefreshSeconds, 10, 3600);
}

void FLodeSettings::Save() const
{
	FScopeLock ScopeLock(&Lock);
	const FString& Ini = SourceControlHelpers::GetSettingsIni();
	GConfig->SetString(Section, TEXT("LodePath"), *LodePath, Ini);
	GConfig->SetInt(Section, TEXT("ServerRefreshSeconds"), ServerRefreshSeconds, Ini);
	GConfig->SetBool(Section, TEXT("KeepLocksDefault"), bKeepLocksDefault, Ini);
	GConfig->Flush(false, Ini);
}
