// Copyright SimpleDevOps contributors. Licensed under Apache-2.0.

using UnrealBuildTool;

public class LodeSourceControl : ModuleRules
{
	public LodeSourceControl(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		IWYUSupport = IWYUSupport.Full;

		PrivateDependencyModuleNames.AddRange(new string[]
		{
			"AssetRegistry",
			"ContentBrowser",
			"Core",
			"CoreUObject",
			"Engine",
			"Json",
			"Slate",
			"SlateCore",
			"InputCore",
			"UnrealEd",
			"SourceControl",
			"SourceControlWindows",
			"Projects",
			"ToolMenus",
		});
	}
}
