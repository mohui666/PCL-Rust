#!/usr/bin/env python3
"""Archive a freshly built launcher with its redistributable resources and notices."""
import argparse
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
NOTICES = {
    "crates/pcl-core/assets/launch/JavaWrapper-LICENCE": "JavaWrapper-LICENCE",
    "crates/pcl-core/assets/launch/LwjglUnsafeAgent-LICENSE": "LwjglUnsafeAgent-LICENSE",
    "crates/pcl-core/assets/launch/SOURCES.md": "LAUNCH-PATCH-SOURCES.md",
    "crates/pcl-desktop/assets/upstream/README-SOURCES.md": "UPSTREAM-RESOURCES.md",
    "crates/pcl-desktop/assets/upstream/SOURCE-MANIFEST.json": "UPSTREAM-SOURCE-MANIFEST.json",
    "crates/pcl-desktop/assets/help/README.md": "HELP-RESOURCES.md",
    "crates/pcl-desktop/assets/help/SOURCE.json": "HELP-SOURCE.json",
    "crates/pcl-core/assets/wiki/SOURCES.md": "WIKI-SOURCES.md",
    "docs/assets/README.md": "ICON-SOURCES.md",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=["windows-x86_64", "macos-arm64", "linux-x86_64"])
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    target = args.platform
    output = ROOT / "dist/release"
    output.mkdir(parents=True, exist_ok=True)
    folders = {
        "windows-x86_64": "PCL-Rust-Windows",
        "macos-arm64": "PCL Rust.app",
        "linux-x86_64": "PCL-Rust-Linux-x86_64",
    }
    package = ROOT / "dist" / folders[target]
    assert package.is_dir(), "Build the platform package first"
    resources = package / "Contents/Resources" if target == "macos-arm64" else package
    (resources / "licenses").mkdir(exist_ok=True)
    for source, name in NOTICES.items():
        shutil.copyfile(ROOT / source, resources / "licenses" / name)
    shutil.copyfile(ROOT / "UPSTREAM-LICENCE", resources / "UPSTREAM-LICENCE")
    shutil.copyfile(ROOT / "docs/release-usage.md", resources / "使用说明.md")
    shutil.copyfile(ROOT / "docs/headless.md", resources / "CLI.md")
    name = f"PCL-Rust-v{version}-{target}"
    if target == "macos-arm64":
        assert platform.machine() == "arm64", "The macOS release must be ARM64"
        assert not list(resources.glob("PingFang*")), "System font derivatives cannot be published"
        assert (resources / "OFL.txt").is_file(), "Missing redistributable font license"
        subprocess.run(["codesign", "--force", "--sign", "-", str(package)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(package)], check=True)
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent",
                        str(package), str(output / f"{name}.zip")], check=True)
    elif target == "linux-x86_64":
        with tarfile.open(output / f"{name}.tar.gz", "w:gz") as archive:
            archive.add(package, arcname=package.name)
    else:
        with zipfile.ZipFile(output / f"{name}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
            for path in sorted(package.rglob("*")):
                if path.is_file():
                    archive.write(path, path.relative_to(package.parent))
    print(f"Packaged {name}")


if __name__ == "__main__":
    main()
