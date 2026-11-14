// The demo depots' folders for the mock backend's view editor: each leaf folder with how many
// files and bytes it holds, and a few file names.

export interface DemoFolder {
  files: number;
  bytes: number;
}

const SKYFALL: Record<string, DemoFolder> = {
  Config: { files: 24, bytes: 310_000 },
  "Content/Characters": { files: 2_140, bytes: 1_850_000_000 },
  "Content/Cinematics": { files: 412, bytes: 3_240_000_000 },
  "Content/Maps": { files: 86, bytes: 2_120_000_000 },
  "Content/Props": { files: 3_920, bytes: 1_410_000_000 },
  "Content/UI": { files: 610, bytes: 240_000_000 },
  Docs: { files: 58, bytes: 12_000_000 },
  "Source/Skyfall": { files: 1_210, bytes: 18_500_000 },
  "Tools/Pipeline": { files: 140, bytes: 4_100_000 },
};

const PIPELINE: Record<string, DemoFolder> = {
  Source: { files: 380, bytes: 2_400_000 },
  Docs: { files: 32, bytes: 600_000 },
};

/** The leaf folders of a demo depot. */
export function demoFolders(depot: string): Record<string, DemoFolder> {
  return depot === "skyfall" ? SKYFALL : PIPELINE;
}

const EXT: Record<string, string> = { Config: "ini", Docs: "md", Source: "cpp", Tools: "py" };

/** A few made-up file names of a leaf folder. */
export function demoFiles(folder: string, n = 3): string[] {
  const top = folder.split("/")[0]!;
  const ext = EXT[top] ?? "uasset";
  const leaf = folder.slice(folder.lastIndexOf("/") + 1);
  return Array.from({ length: n }, (_, i) => `${folder}/${leaf}_${String(i + 1).padStart(2, "0")}.${ext}`);
}
