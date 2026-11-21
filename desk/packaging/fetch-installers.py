"""Fetch release acceptance fixtures on a GitHub-hosted runner."""

import hashlib
import json
import os
import re
import subprocess
import zipfile
from pathlib import Path

repo = os.environ["GITHUB_REPOSITORY"]
root = Path(os.environ["RUNNER_TEMP"]) / "installer-files"
release_version = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True)


def download(tag, folder, *patterns):
    folder.mkdir(parents=True, exist_ok=True)
    args = ["release", "download", tag, "--repo", repo, "--dir", str(folder)]
    for pattern in patterns:
        args.extend(["--pattern", pattern])
    gh(*args)


def verify(path, checksums):
    for line in checksums.read_text(encoding="utf-8").splitlines():
        checksum, name = line.split(maxsplit=1)
        if name.lstrip("*") == path.name:
            with path.open("rb") as source:
                if (
                    checksum.lower()
                    == hashlib.file_digest(source, "sha256").hexdigest()
                ):
                    return
            raise ValueError(f"Checksum mismatch for {path.name}")
    raise ValueError(f"No checksum for {path.name}")


def parts(version):
    return tuple(map(int, version.split(".")))


def main():
    candidate = root / "desk"
    run = os.environ.get("CANDIDATE_RUN")
    if run:
        if not re.fullmatch(r"[0-9]+", run):
            raise ValueError("candidate_run must be a numeric desk-release run ID")
        metadata = json.loads(gh("api", f"repos/{repo}/actions/runs/{run}"))
        if (
            metadata["path"] != ".github/workflows/desk-release.yml"
            or metadata["conclusion"] != "success"
            or metadata["head_repository"]["full_name"] != repo
        ):
            raise ValueError(
                "Expected a successful desk-release run from this repository"
            )
        gh(
            "run",
            "download",
            run,
            "--repo",
            repo,
            "--name",
            "lode-desk-release",
            "--dir",
            str(candidate),
        )
    else:
        download("lode-desk-feed", candidate, "latest.json")

    version = json.loads((candidate / "latest.json").read_text(encoding="utf-8"))[
        "version"
    ]
    if not re.fullmatch(release_version, version):
        raise ValueError("Expected a stable Desk version in latest.json")
    windows = os.environ["RUNNER_OS"] == "Windows"
    suffixes = (
        ["windows-x86_64.msi", "windows-x86_64-setup.exe"]
        if windows
        else ["linux-x86_64.deb"]
    )
    for suffix in suffixes:
        name = f"lode-desk-{version}-{suffix}"
        if not run:
            download(f"desk-v{version}", candidate, name, name + ".sha256")
        verify(candidate / name, candidate / (name + ".sha256"))

    env = {
        "DESK_VERSION": version,
        "CANDIDATE_FILES": candidate,
        "ACCEPTANCE_STAGE": "candidate" if run else "published",
    }
    if windows:
        releases = gh(
            "api",
            "--paginate",
            f"repos/{repo}/releases?per_page=100",
            "--jq",
            ".[] | select(.draft == false and .prerelease == false) | "
            "{tag: .tag_name, assets: [.assets[].name]}",
        )
        # gh prints one compact JSON object per line with a non-interactive stdout.
        older = {}
        cores = {}
        for line in releases.splitlines():
            release = json.loads(line)
            tag = release["tag"]
            match = re.fullmatch(rf"(desk|lode)-v({release_version})", tag)
            if not match:
                continue
            v = match[2]
            msi = f"lode-desk-{v}-windows-x86_64.msi"
            if parts(v) < parts(version) and msi in release["assets"]:
                older[tag] = (parts(v), msi)
            archive = f"lode-{v}-x86_64-pc-windows-msvc.zip"
            if match[1] == "lode" and archive in release["assets"]:
                cores[tag] = (parts(v), archive)

        previous = os.environ.get("PREVIOUS_TAG") or max(older, key=older.get)
        core = os.environ.get("CORE_TAG") or max(cores, key=cores.get)
        if previous not in older or core not in cores:
            raise ValueError(
                "Check previous_tag and core_tag against the published release assets"
            )
        msi = older[previous][1]
        archive = cores[core][1]
        prior, server = root / "previous", root / "core"
        download(previous, prior, msi, msi + ".sha256")
        verify(prior / msi, prior / (msi + ".sha256"))
        download(core, server, archive, "SHA256SUMS")
        verify(server / archive, server / "SHA256SUMS")
        with zipfile.ZipFile(server / archive) as package:
            package.extractall(server / "programs")
        env.update(PREVIOUS_MSI=prior / msi, CORE_PROGRAMS=server / "programs")
        print(f"Checking {previous} to Desk {version} against {core}")
    with Path(os.environ["GITHUB_ENV"]).open("a", encoding="utf-8") as output:
        for key, value in env.items():
            print(f"{key}={value}", file=output)


if __name__ == "__main__":
    main()
