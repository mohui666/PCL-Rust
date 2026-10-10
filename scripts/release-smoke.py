#!/usr/bin/env python3
"""Check a release CLI on a fresh CI runner without runtime API key configuration."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

APP_ID = "2c86a114-ddd8-468b-82ca-441923527e09"
FILE_ID = "cf:306612:8786256"
FILE_SHA1 = "810b2b0195371a012906241d8b85a32a1d6de53c"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    cli = args.cli.resolve()
    environment = os.environ.copy()
    for key in ("PCL_CURSEFORGE_API_KEY", "PCL_BUNDLED_CURSEFORGE_API_KEY"):
        environment.pop(key, None)
    version = subprocess.check_output([str(cli), "--version"], env=environment, text=True).strip()
    assert version == f"pcl-cli {args.version}", "Unexpected executable version"
    with tempfile.TemporaryDirectory(prefix="pcl-release-smoke-") as temporary:
        root = Path(temporary)
        base = [str(cli), "--config", str(root / "settings.json"), "--root", str(root), "--json"]

        def run(*command):
            result = subprocess.run(base + list(command), env=environment, capture_output=True,
                                    text=True, encoding="utf-8", timeout=180)
            if result.returncode:
                raise RuntimeError(f"Release CLI {command[0]} failed: {result.stderr[-2000:]}")
            return json.loads(result.stdout)

        settings = run("settings")
        assert settings["microsoft_client_id"] == APP_ID, "The default App ID is missing"
        search = run("search", "Fabric API", "--provider", "curseforge", "--minecraft", "1.21.1",
                     "--loader", "fabric", "--limit", "5")
        assert search["hits"], "CurseForge search returned no results"
        destination = root / "fabric-api.jar"
        result = run("resource", "download", FILE_ID, str(destination))
        assert result["version"] == FILE_ID
        sha1 = hashlib.sha1(destination.read_bytes()).hexdigest()
        assert sha1 == FILE_SHA1, "The downloaded file does not match its publisher's SHA-1"
        report = {
            "version": args.version,
            "platform": platform.system(),
            "architecture": platform.machine(),
            "microsoft_client_id": APP_ID,
            "runtime_key_environment": False,
            "runner_has_user_credentials": False,
            "curseforge_search_hits": len(search["hits"]),
            "curseforge_file_id": FILE_ID,
            "download_bytes": destination.stat().st_size,
            "download_sha1": sha1,
            "cli_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(),
            "microsoft_interactive_login_tested": False,
            "desktop_gui_tested": False,
        }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
