// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

#pragma once

#if WITH_DEV_AUTOMATION_TESTS

#include "CoreMinimal.h"
#include "Async/Async.h"
#include "Dom/JsonObject.h"
#include "HAL/FileManager.h"
#include "Interfaces/IPluginManager.h"
#include "Misc/FileHelper.h"
#include "Misc/Guid.h"
#include "Misc/Paths.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Misc/AutomationTest.h"

#include "LodeRunner.h"

/** Every Lode test runs in the editor and is listed under the engine filter. */
#define LODE_TEST_FLAGS (EAutomationTestFlags::EditorContext | EAutomationTestFlags::EngineFilter)

/** Shared by the plugin's automation tests: the fake lode, its scenarios, and threads. */
namespace LodeTest
{

	inline FString PluginDir()
	{
		const TSharedPtr<IPlugin> Plugin = IPluginManager::Get().FindPlugin(TEXT("LodeSourceControl"));
		return Plugin.IsValid() ? FPaths::ConvertRelativePathToFull(Plugin->GetBaseDir()) : FString();
	}

	inline FString FakeDir() { return PluginDir() / TEXT("Tests") / TEXT("FakeLode"); }
	inline FString Scenario(const FString& Name) { return FakeDir() / TEXT("scenarios") / (Name + TEXT(".json")); }

	/** The Python that ships with the engine, or one on PATH. */
	inline FString FindPython()
	{
		const FString ThirdParty = FPaths::ConvertRelativePathToFull(FPaths::EngineDir() / TEXT("Binaries") / TEXT("ThirdParty") / TEXT("Python3"));
#if PLATFORM_WINDOWS
		const FString Bundled = ThirdParty / TEXT("Win64") / TEXT("python.exe");
#elif PLATFORM_MAC
		const FString Bundled = ThirdParty / TEXT("Mac") / TEXT("bin") / TEXT("python3");
#else
		const FString Bundled = ThirdParty / TEXT("Linux") / TEXT("bin") / TEXT("python3");
#endif
		if (FPaths::FileExists(Bundled))
		{
			return Bundled;
		}
		FString OnPath = FLodeRunner::ResolveExecutable(TEXT("python3"));
		return OnPath.IsEmpty() ? FLodeRunner::ResolveExecutable(TEXT("python")) : OnPath;
	}

	/** A fresh folder under the project's Intermediate folder, removed by the caller. */
	inline FString MakeTempDir(const TCHAR* Name)
	{
		const FString Dir = FPaths::ConvertRelativePathToFull(FPaths::ProjectIntermediateDir() / TEXT("LodeTests") / FString::Printf(TEXT("%s-%s"), Name, *FGuid::NewGuid().ToString(EGuidFormats::Short)));
		IFileManager::Get().MakeDirectory(*Dir, true);
		return Dir;
	}

	inline void RemoveDir(const FString& Dir)
	{
		IFileManager::Get().DeleteDirectory(*Dir, false, true);
	}

	/** Runner settings that run the fake with a scenario, logging each call to LogPath. */
	inline FLodeRunnerConfig FakeConfig(const FString& ScenarioName, const FString& LogPath, const FString& WorkingDirectory)
	{
		FLodeRunnerConfig Config;
		Config.Executable = FindPython();
		Config.LeadingArgs = { FakeDir() / TEXT("fake_lode.py"), TEXT("--scenario"), Scenario(ScenarioName), TEXT("--log"), LogPath };
		Config.WorkingDirectory = WorkingDirectory;
		Config.TempDirectory = WorkingDirectory / TEXT("tmp");
		return Config;
	}

	/** Runs a function on a separate thread and waits: lode calls are refused on the game thread. */
	template <typename FunctionType>
	auto OffGameThread(FunctionType&& Function) -> decltype(Function())
	{
		return Async(EAsyncExecution::Thread, Forward<FunctionType>(Function)).Get();
	}

	/** One JSON line per fake call: {"args": [...], "stdin": "...", "paths": [...]}. */
	inline TArray<TSharedPtr<FJsonObject>> ReadLog(const FString& LogPath)
	{
		TArray<TSharedPtr<FJsonObject>> Calls;
		TArray<FString> Lines;
		FFileHelper::LoadFileToStringArray(Lines, *LogPath);
		for (const FString& Line : Lines)
		{
			TSharedPtr<FJsonObject> Object;
			if (FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Line), Object) && Object.IsValid())
			{
				Calls.Add(Object);
			}
		}
		return Calls;
	}

	/** The recorded answer in a scenario whose pattern is Pattern (tokens joined by spaces), with {root} replaced. */
	inline TSharedPtr<FJsonObject> Answer(const FString& ScenarioName, const FString& Pattern, const FString& Root)
	{
		FString Text;
		if (!FFileHelper::LoadFileToString(Text, *Scenario(ScenarioName)))
		{
			return nullptr;
		}
		TSharedPtr<FJsonObject> Json;
		if (!FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Text), Json) || !Json.IsValid())
		{
			return nullptr;
		}
		for (const TSharedPtr<FJsonValue>& Call : Json->GetArrayField(TEXT("calls")))
		{
			const TSharedPtr<FJsonObject> CallObject = Call->AsObject();
			TArray<FString> Tokens;
			CallObject->TryGetStringArrayField(TEXT("args"), Tokens);
			if (FString::Join(Tokens, TEXT(" ")) == Pattern)
			{
				// Re-serialize with the root filled in, as the fake would print it.
				FString Out;
				FJsonSerializer::Serialize(CallObject->GetObjectField(TEXT("stdout")).ToSharedRef(), TJsonWriterFactory<>::Create(&Out));
				Out.ReplaceInline(TEXT("{root}"), *Root);
				TSharedPtr<FJsonObject> Answer;
				FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Out), Answer);
				return Answer;
			}
		}
		return nullptr;
	}
}

#endif
