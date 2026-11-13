//! Project templates for `lode init` (T085): a `.lodeignore` and a `.lodeconfig` for common
//! kinds of project, built into the binary.

/// One template.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    /// Name given to `--template`.
    pub name: &'static str,
    /// What it is for.
    pub about: &'static str,
    /// The `.lodeignore` text.
    pub ignore: &'static str,
    /// The `.lodeconfig` text.
    pub config: &'static str,
}

/// Lines every template ignores: operating system and editor leftovers.
const COMMON_IGNORE: &str = "\
# Operating system and editor leftovers
.DS_Store
Thumbs.db
desktop.ini
*.tmp
*.swp
*~
~$*
";

macro_rules! ignore {
    ($($line:expr),* $(,)?) => {
        concat!($($line, "\n",)* "\n# Operating system and editor leftovers\n.DS_Store\nThumbs.db\ndesktop.ini\n*.tmp\n*.swp\n*~\n~$*\n")
    };
}

/// Every built-in template.
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "generic",
        about: "any project: only operating system and editor leftovers are ignored",
        ignore: COMMON_IGNORE,
        config: "\
# Lode depot settings (see the user guide, \"Lock rules\").
[locks]
# Files that need `lode lock` before they are changed, for example:
# required = [\"**/*.psd\", \"**/*.fbx\"]
required = []
",
    },
    Template {
        name: "unreal",
        about: "Unreal Engine: build output and caches ignored, assets and maps need a lock",
        ignore: ignore!(
            "# Unreal Engine build output and caches",
            "Binaries/",
            "Intermediate/",
            "Saved/",
            "DerivedDataCache/",
            "Plugins/**/Binaries/",
            "Plugins/**/Intermediate/",
            ".vs/",
            ".vsconfig",
            "*.sln",
            "*.suo",
            "*.VC.db",
            "*.opensdf",
            "*.sdf",
        ),
        config: "\
# Lode depot settings for an Unreal Engine project.
[locks]
# Binary assets cannot be merged: take a lock before changing one.
required = [\"**/*.uasset\", \"**/*.umap\"]

[files]
binary = [\"**/*.uasset\", \"**/*.umap\", \"**/*.ubulk\", \"**/*.uexp\"]
",
    },
    Template {
        name: "unity",
        about: "Unity: generated folders ignored, scenes, prefabs and art need a lock",
        ignore: ignore!(
            "# Unity generated folders and files",
            "/[Ll]ibrary/",
            "/[Tt]emp/",
            "/[Oo]bj/",
            "/[Bb]uild/",
            "/[Bb]uilds/",
            "/[Ll]ogs/",
            "/[Uu]ser[Ss]ettings/",
            "/[Mm]emoryCaptures/",
            ".vs/",
            "*.csproj",
            "*.sln",
            "*.pidb",
            "*.booproj",
            "*.unityproj",
        ),
        config: "\
# Lode depot settings for a Unity project.
[locks]
# Scenes and prefabs merge badly; art files cannot be merged.
required = [\"**/*.unity\", \"**/*.prefab\", \"**/*.psd\", \"**/*.fbx\"]

[files]
binary = [\"**/*.psd\", \"**/*.fbx\", \"**/*.png\", \"**/*.wav\"]
",
    },
    Template {
        name: "godot",
        about: "Godot 4: the import cache ignored, source art needs a lock",
        ignore: ignore!(
            "# Godot import cache and exports",
            ".godot/",
            ".import/",
            "/android/",
            "export_credentials.cfg",
        ),
        config: "\
# Lode depot settings for a Godot project.
[locks]
# Source art cannot be merged: take a lock before changing it.
required = [\"**/*.blend\", \"**/*.psd\", \"**/*.kra\"]
",
    },
    Template {
        name: "rust",
        about: "Rust: Cargo's target directory ignored",
        ignore: ignore!("# Cargo build output", "target/"),
        config: "\
# Lode depot settings for a Rust project.
[locks]
required = []
",
    },
    Template {
        name: "node",
        about: "Node.js: installed packages, build output and local secrets ignored",
        ignore: ignore!(
            "# Installed packages, build output, local secrets",
            "node_modules/",
            "dist/",
            "build/",
            "coverage/",
            ".env",
            ".env.local",
            "npm-debug.log*",
            "yarn-error.log*",
        ),
        config: "\
# Lode depot settings for a Node.js project.
[locks]
required = []
",
    },
];

/// The template called `name`.
pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// The templates, one per line with what each is for, for messages.
pub fn names() -> String {
    TEMPLATES
        .iter()
        .map(|t| format!("\n  {:<8} {}", t.name, t.about))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lode_core::config::LodeConfig;
    use lode_core::path::DepotPath;
    use lode_workspace::IgnoreStack;
    use std::path::Path;

    /// (template, path, ignored, lock required)
    const SAMPLES: &[(&str, &str, bool, bool)] = &[
        ("generic", "notes/readme.md", false, false),
        ("generic", "art/.DS_Store", true, false),
        ("generic", "Thumbs.db", true, false),
        ("unreal", "Binaries/Win64/Game.exe", true, false),
        ("unreal", "Intermediate/Build/x.obj", true, false),
        ("unreal", "Saved/Logs/Game.log", true, false),
        ("unreal", "DerivedDataCache/a.ddp", true, false),
        (
            "unreal",
            "Plugins/Tool/Binaries/Win64/Tool.dll",
            true,
            false,
        ),
        ("unreal", "Game.sln", true, false),
        ("unreal", "Content/Maps/Main.umap", false, true),
        ("unreal", "Content/Hero/Hero.uasset", false, true),
        ("unreal", "Source/Game/Game.cpp", false, false),
        ("unreal", "Game.uproject", false, false),
        ("unity", "Library/ArtifactDB", true, false),
        ("unity", "Temp/x", true, false),
        ("unity", "Assembly-CSharp.csproj", true, false),
        ("unity", "Assets/Scenes/Main.unity", false, true),
        ("unity", "Assets/Prefabs/Hero.prefab", false, true),
        ("unity", "Assets/Scripts/Hero.cs", false, false),
        ("unity", "Assets/Library/notes.txt", false, false),
        ("godot", ".godot/imported/x.ctex", true, false),
        ("godot", "art/hero.blend", false, true),
        ("godot", "scenes/main.tscn", false, false),
        ("rust", "target/debug/app", true, false),
        ("rust", "src/main.rs", false, false),
        ("node", "node_modules/left-pad/index.js", true, false),
        ("node", ".env", true, false),
        ("node", "src/index.ts", false, false),
    ];

    fn ignored(t: &Template, path: &str) -> bool {
        let root = Path::new("/w");
        let (stack, problem) = IgnoreStack::with_text(&IgnoreStack::empty(), root, t.ignore);
        assert!(problem.is_none(), "{}: {problem:?}", t.name);
        // Ignoring a parent directory ignores everything below it, as the scan does.
        let mut cur = root.to_path_buf();
        let parts: Vec<&str> = path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            cur.push(part);
            if stack.is_ignored(&cur, i + 1 < parts.len()) {
                return true;
            }
        }
        false
    }

    #[test]
    fn every_template_parses_and_matches_its_samples() {
        for t in TEMPLATES {
            let config = LodeConfig::parse(t.config.as_bytes()).unwrap();
            for (_, path, ignore, lock) in SAMPLES.iter().filter(|s| s.0 == t.name) {
                assert_eq!(ignored(t, path), *ignore, "{} ignore {path}", t.name);
                let p = DepotPath::parse(path).unwrap();
                assert_eq!(
                    config.lock_required.matches(&p),
                    *lock,
                    "{} lock {path}",
                    t.name
                );
            }
            assert!(
                t.ignore.contains("Thumbs.db"),
                "{} has the common lines",
                t.name
            );
        }
        assert!(TEMPLATES
            .iter()
            .all(|t| SAMPLES.iter().any(|s| s.0 == t.name)));
        assert!(find("unreal").is_some() && find("cobol").is_none());
    }
}
