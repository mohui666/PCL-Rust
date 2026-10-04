"""Rebuild the inert, offline help snapshot from the fixed local PCL source.

No XAML is evaluated. Only display attributes and link data are retained.
Run from the repository root: python3 crates/pcl-desktop/assets/help/build_catalog.py
"""
from pathlib import Path, PurePosixPath
import hashlib
import json
import shutil
import subprocess
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[4]
SOURCE = ROOT / "test-output/upstream/Plain Craft Launcher 2"
DEST = Path(__file__).parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def node(element):
    # XML comments are discarded by ElementTree. Event collections are data,
    # never callable; the Rust renderer accepts only a small action allowlist.
    tag = element.tag.rsplit("}", 1)[-1]
    attrs = {k.rsplit("}", 1)[-1]: v for k, v in element.attrib.items()}
    keep = {"Text", "Title", "Info", "Source", "FontSize", "FontWeight",
            "CanSwap", "IsSwapped", "IsWarn", "EventType", "EventData"}
    attrs = {k.rsplit(".", 1)[-1]: v for k, v in attrs.items()
             if k.rsplit(".", 1)[-1] in keep}
    return {"tag": tag, "attrs": attrs, "children": [node(c) for c in element]}


archive = SOURCE / "Resources/Help.zip"
entries = []
files = []
with zipfile.ZipFile(archive) as z:
    for entry in z.infolist():
        path = PurePosixPath(entry.filename)
        assert not path.is_absolute() and ".." not in path.parts
        files.append({"path": entry.filename, "sha256": hashlib.sha256(z.read(entry)).hexdigest()})
        if path.suffix != ".json":
            continue
        meta = json.loads(z.read(entry).decode("utf-8-sig"))
        xaml = str(path.with_suffix(".xaml"))
        nodes = []
        if not meta.get("IsEvent", False):
            text = z.read(xaml).decode("utf-8-sig")
            root = ET.fromstring('<Root xmlns:local="local" xmlns:x="x">' + text + '</Root>')
            nodes = [node(n) for n in root]
        entries.append({"id": str(path.with_suffix("")), "meta": meta, "nodes": nodes})

about_file = SOURCE / "Pages/PageOther/PageOtherAbout.xaml"
about = ET.parse(about_file).getroot()
cards = [n for n in about.iter() if n.tag.endswith("}MyCard")]
thanks = []
for card in cards:
    if card.get("Title") != "特别鸣谢":
        continue
    for el in card.iter():
        if not el.tag.endswith("}MyListItem"):
            continue
        row = el.get("Grid.Row", "0")
        image = next(n for n in card.iter() if n.tag.endswith("}Image") and n.get("Grid.Row", "0") == row)
        button = next((n for n in card.iter() if n.tag.endswith("}MyButton") and n.get("Grid.Row", "0") == row), None)
        thanks.append({"title": el.get("Title"), "info": el.get("Info"),
                       "head": image.get("Source").rsplit("/", 1)[-1],
                       "button": button.get("Text", "") if button is not None else "",
                       "url": next((v for k,v in button.attrib.items() if k.endswith("EventData")), "") if button is not None else ""})
names = []
for card in cards:
    if card.get("Title") == "赞助者":
        for wrap in card.iter():
            if wrap.tag.endswith("}WrapPanel"):
                names.extend(n.get("Text") for n in wrap if n.get("Text"))
licenses = [node(n) for n in cards if n.get("Title") == "许可与版权声明"]

left = SOURCE / "Pages/PageOther/PageOtherLeft.xaml"
icons = {}
for el in ET.parse(left).iter():
    name = el.get("{http://schemas.microsoft.com/winfx/2006/xaml}Name", "")
    if name.startswith("Item") and el.get("Logo"):
        icons[name[4:].lower()] = el.get("Logo")
    if el.tag.endswith("}MyIconButton"):
        icons["refresh"] = el.get("Logo")

heads = DEST / "heads"
heads.mkdir(parents=True, exist_ok=True)
head_files = []
for path in sorted((SOURCE / "Images/Heads").iterdir()):
    shutil.copyfile(path, heads / path.name)
    head_files.append({"path": "Images/Heads/" + path.name, "sha256": digest(path)})
data = {"entries": entries, "thanks": thanks, "sponsors": names,
        "upstream_licenses": licenses, "icons": icons}
(DEST / "catalog.json").write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
manifest = {"source": "https://github.com/Meloong-Git/PCL", "reference_version": "2.13.1.1",
            "upstream_commit": subprocess.check_output(["git", "-C", str(ROOT / "test-output/upstream"), "rev-parse", "HEAD"], text=True).strip(),
            "license": "../../../../UPSTREAM-LICENCE",
            "help_archive_sha256": digest(archive), "entries": len(entries), "archive_files": len(files),
            "help_files": files, "heads": head_files,
            "about_sha256": digest(about_file), "sidebar_sha256": digest(left),
            "catalog_sha256": digest(DEST / "catalog.json")}
(DEST / "SOURCE.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
print(f"Embedded {len(entries)} entries, {len(thanks)} credits, {len(names)} sponsor names; no remote images fetched.")
