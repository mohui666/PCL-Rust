#!/usr/bin/env python3
"""Prepare redistributable TrueType CJK fonts for the Linux package."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys
import urllib.request

try:
    import fontTools
    from fontTools.ttLib import TTFont
    from fontTools.varLib.instancer import instantiateVariableFont
except ImportError:
    sys.exit("Install fontTools first: python3 -m pip install fonttools==4.60.1")

ROOT = Path(__file__).resolve().parent.parent
COMMIT = "a85815a42757630ce188fdad368c2dfc444d4773"
BASE = f"https://raw.githubusercontent.com/google/fonts/{COMMIT}/ofl/notosanssc"
INPUTS = {
    "NotoSansSC-VF.ttf": (
        f"{BASE}/NotoSansSC%5Bwght%5D.ttf",
        "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da",
    ),
    "OFL.txt": (
        f"{BASE}/OFL.txt",
        "1c05c68c34f9708415aada51f17e1b0092d2cea709bf4a94cd38114f9e73d7d9",
    ),
}
SAMPLE = "启动下载设置游戏版本资源关闭取消语言帮助检查微软账户安装整合包"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def download(cache, name, url, expected):
    path = cache / name
    if not path.exists():
        with urllib.request.urlopen(url, timeout=120) as response:
            data = response.read()
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f"Download checksum mismatch: {name}")
        path.write_bytes(data)
    if digest(path) != expected:
        raise ValueError(f"Cached input checksum mismatch: {path}")
    return path


def verify(path, weight):
    with TTFont(path) as font:
        if "glyf" not in font or "fvar" in font:
            raise ValueError(f"Expected static TrueType outlines: {path}")
        if font["OS/2"].usWeightClass != weight:
            raise ValueError(f"Wrong font weight: {path}")
        cmap = font.getBestCmap()
        missing = [char for char in SAMPLE if ord(char) not in cmap]
        if missing:
            raise ValueError(f"Missing Chinese glyphs in {path}: {''.join(missing)}")
        for char in SAMPLE:
            glyph = font["glyf"][cmap[ord(char)]]
            if glyph.numberOfContours == 0:
                raise ValueError(f"Missing Chinese outline in {path}: {char}")
    return {"file": path.name, "weight": weight, "sha256": digest(path)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "test-output/fonts/linux")
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "test-output/linux-build-inputs")
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    output = args.output_dir
    if not args.verify_only:
        args.cache_dir.mkdir(parents=True, exist_ok=True)
        source = {
            name: download(args.cache_dir, name, url, sha)
            for name, (url, sha) in INPUTS.items()
        }
        output.mkdir(parents=True, exist_ok=True)
        for style, weight in (("Regular", 400), ("Semibold", 600)):
            destination = output / f"NotoSansSC-{style}.ttf"
            if not destination.exists():
                print(f"Preparing Noto Sans SC {style}…", flush=True)
                with TTFont(source["NotoSansSC-VF.ttf"], recalcTimestamp=False) as variable:
                    font = instantiateVariableFont(variable, {"wght": weight}, inplace=True)
                    # The source's default face is Thin; static face names must match its weight.
                    for name_id, value in ((1, "Noto Sans SC"), (2, style),
                                           (4, f"Noto Sans SC {style}"),
                                           (6, f"NotoSansSC-{style}")):
                        font["name"].setName(value, name_id, 3, 1, 0x409)
                    font.save(destination)
        shutil.copyfile(source["OFL.txt"], output / "OFL.txt")
    results = [verify(output / f"NotoSansSC-{style}.ttf", weight)
               for style, weight in (("Regular", 400), ("Semibold", 600))]
    if digest(output / "OFL.txt") != INPUTS["OFL.txt"][1]:
        raise ValueError("The bundled OFL license does not match the pinned source")
    report = {
        "family": "Noto Sans SC",
        "source": f"https://github.com/google/fonts/tree/{COMMIT}/ofl/notosanssc",
        "source_sha256": INPUTS["NotoSansSC-VF.ttf"][1],
        "license": "SIL Open Font License 1.1 (OFL.txt)",
        "transformation": "TrueType variable font instantiated at wght=400 and wght=600",
        "fonttools_version": fontTools.__version__,
        "outline_format": "TrueType glyf",
        "sample_glyphs": SAMPLE,
        "fonts": results,
    }
    if not args.verify_only:
        (output / "FONT-SOURCES.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
