# Lode for Unreal: troubleshooting

Every message below is what the plugin shows in the Revision Control settings or in the Message Log (**Window > Message Log > Revision Control**). For more detail, run the editor with `-LogCmds="LogLodeSourceControl Verbose"`: the Output Log then lists each `lode` command (arguments only, never a token or password) and how long it took.

| You see | Why | What to do |
| --- | --- | --- |
| "The lode program was not found" | `lode` is not on `PATH` for the editor, or the path in the settings is wrong | Install Lode, then restart the editor (and the Epic launcher) so it sees the new `PATH`, or put the full path to `lode.exe` in **Path to lode** |
| "lode 0.1.x is too old for this plugin" | The plugin needs lode 0.2.0 or later | Update with `lode self-update`, or install the newer release |
| "This project is not inside a Lode workspace" | No `.lode` folder in the project folder or any folder above it | Check out the depot so the `.uproject` is inside it, or start one with `lode init <server>/<depot> --template unreal` (section 2 of the guide) |
| "You are not signed in to the Lode server" | No stored sign-in for this server, or it expired | Sign in from the settings panel, or run `lode login <server>` in a terminal, then press **Accept Settings** |
| "Your Lode token has no 'lock' scope" | The token was made without the `lock` scope (the first admin token from `loded init` is one such) | Ask for a token with the `read`, `write`, `lock` and `shelve` scopes, or sign in with your name and password |
| "Locked by someone else: PATH (held by NAME)" when checking out, or "Locked by NAME" on the asset | Someone else holds the lock | Ask them to submit or revert. Only an administrator can break a lock (`lode unlock --force`) |
| "These files need a lock before they can be submitted" | The asset was made writable outside the editor and changed without a lock | Check it out (take the lock), then submit |
| "Newer versions of these files are on the server" | Someone submitted the same files since your last sync | **Sync Content**, resolve any conflicts, then submit again |
| Sync reports conflicts | The same file changed here and on the server | For assets: right-click > **Keep My Version** or **Take Their Version** (in the "Lode conflict" section). For text files: fix the conflict markers, then mark resolved |
| "... Check that the Lode server is running and reachable." | Network, VPN or the server is down | Check the server address in the settings panel; `lode whoami` in a terminal shows the same problem with more detail |
| "Another lode command is working in this workspace" | A `lode` command in a terminal or in Lode Desk holds the workspace | Wait for it to finish and try again |
| Asset icons never show locks held by others | Server information is refreshed for the assets you are looking at, every minute by default | Wait, or use **Refresh** in the Content Browser's revision control menu; lower "Refresh locks every" if needed |
| A file outside the project shows as not controlled | Only files inside the Lode workspace are managed | Expected: engine and plugin content outside the workspace are left alone |

**Logs to send with a bug report.** `Saved/Logs/<Project>.log` from the project folder (search for `LogLodeSourceControl`), and the output of `lode status --json` and `lode --version --json` from the workspace.
