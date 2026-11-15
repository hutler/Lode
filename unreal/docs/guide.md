# Lode for Unreal: guide

This guide takes you from a clean machine to submitting an asset from the Unreal Editor with Lode. It covers Unreal Engine 5.4 and later on Windows; Linux and macOS work the same way with the paths changed.

If something goes wrong, see [troubleshooting.md](troubleshooting.md).

## 1. What you need

- Unreal Engine 5.4 or later.
- The `lode` command, version 0.2.0 or later. Install it from the Lode release page (the Windows installer puts `lode` on `PATH`). Check with `lode --version` in a new terminal.
- A Lode server you can reach, and a user on it. Ask whoever runs the server for its address (such as `https://lode.example.com`) and for a token, or a name and password.

## 2. Put the project in a Lode workspace

The plugin works on the Lode workspace that holds your `.uproject` file. There are two ways to get one.

**The depot already holds the project.** Check it out:

```
lode login https://lode.example.com
lode checkout https://lode.example.com/game C:\Work\Game
```

**You are adding an existing project to a new depot.** From the project folder, create the depot with the Unreal template (needs a server administrator):

```
cd C:\Work\Game
lode init https://lode.example.com/game --template unreal -m "Import"
```

The template writes two files that every Unreal depot should have:

- `.lodeignore` leaves out `Binaries/`, `Intermediate/`, `Saved/`, `DerivedDataCache/` and other generated folders.
- `.lodeconfig` marks `.uasset` and `.umap` files as needing a lock. Lode keeps those files read-only on disk until you lock them, because binary assets cannot be merged.

## 3. Install the plugin

1. Unzip `LodeSourceControl-UE5.x-Win64.zip` (matching your engine version).
2. Copy the `LodeSourceControl` folder into your project's `Plugins` folder, so that `Plugins/LodeSourceControl/LodeSourceControl.uplugin` exists. To use it in every project, copy it to `Engine/Plugins/Marketplace` instead.
3. Start the editor. If it asks to enable the plugin, say yes.

## 4. Connect

1. In the editor, open **Revision Control** at the bottom right (or **Tools > Revision Control > Connect to Revision Control**).
2. Choose **Lode** as the provider.
3. The panel shows the workspace, server, depot, branch and user that the plugin found. If `lode` is not on `PATH`, enter its full path in **Path to lode**.
4. If you have not signed in with `lode login` yet, sign in here with a token, with your name and password, or with **Sign in with the browser** if your company uses single sign-on. Lode keeps the sign-in; the editor never stores your token or password.
5. Press **Accept Settings**. The Message Log (Revision Control) says "Connected to Lode: ..." with your name, server, depot and branch.

## 5. Daily work

**See who has what.** Asset icons in the Content Browser show their state: locked by you, locked by someone else (hover to see who), needs a lock, newer on the server, added, modified, conflicted. The plugin asks the server for locks and newer versions about once a minute for the assets you are looking at; change that in the settings.

**Edit an asset.** Open it and change it. Because the file is read-only, the editor offers to **Check Out**. Checking out takes the Lode lock; no one else can then lock the asset until you submit or revert. If someone else holds the lock, the editor tells you who.

Text files such as `.ini` files need no lock: edit them and they show as modified.

**Get the latest.** **Revision Control > Sync Content** runs `lode update`. Assets that changed on the server are reloaded. If an asset changed both here and on the server, it becomes conflicted: right-click it in the Content Browser and choose **Keep My Version** or **Take Their Version** in the "Lode conflict" section. Text files that conflict get the usual conflict markers; fix the file, then mark it resolved.

**Submit.** **Revision Control > Submit Content** (or **View Changes**, then **Submit**). Write a description and submit. Your locks are released unless you tick **Keep Files Checked Out** (or set "Keep locks after submit" in the settings).

**Revert.** Right-click > **Revision Control > Revert** throws away your change and releases the lock.

**History and diff.** Right-click > **Revision Control > History** lists every submit of the asset, following renames. Select a revision and choose **Diff Against Previous** or **Diff Against Workspace**: the plugin fetches that version and the editor's own asset diff compares them.

**Rename and move.** Rename or move assets in the Content Browser as usual. The plugin records the move so history follows the asset, and adds the redirector the editor leaves behind. Submit the redirector together with the asset; fix up redirectors later as you normally would.

## 6. Changelists and shelves

**View Changes** (Tools > View Changes) lists your changes by changelist. Create changelists, drag files between them, and submit one changelist alone.

**Shelve** a changelist to put its changes on the server without submitting them, for review or to pass work to someone else. Shelving from the default changelist first moves the files into a new changelist named after the description. Shelving the same changelist again updates its shelf.

**Tools > Lode Shelves** lists everyone's open shelves. Select one to see its files, then **Unshelve Into My Workspace** to bring its changes into your workspace.

## 7. Settings

**Edit > Editor Preferences** is not used; all settings are in the Revision Control dialog:

| Setting | What it does |
| --- | --- |
| Path to lode | The `lode` program. Empty means the one on `PATH`, then the usual install folders |
| Refresh locks every (seconds) | How often the server is asked about locks and newer versions (10 to 3600; default 60) |
| Keep locks after submit | Keep your locks on submitted assets by default |

The settings are saved per project in `Saved/Config/<Platform>/SourceControlSettings.ini`, and never contain a token or password.

## 8. For plugin developers

- Build a zip: `Scripts/BuildPlugin.ps1 -Engine "C:\Program Files\Epic Games\UE_5.4"`.
- Run the tests: `Scripts/RunTests.ps1 -Engine ...` runs every `Lode.*` automation test against a fake `lode` (`LodeSourceControl/Tests/FakeLode/fake_lode.py`, run with the engine's own Python).
- End-to-end tests: `Scripts/RunTests.ps1 -Engine ... -E2E -Lode C:\path\lode.exe -Loded C:\path\loded.exe -Repeat 3` starts a real server with two users each time and runs `Lode.E2E`.
- After a Lode release, refresh the fake's recorded answers with `Tests/FakeLode/record_scenarios.py --lode ... --loded ...` and run the tests again.
