#!/usr/bin/env python3
"""Dependency-free local CoreText outline -> sfnt TrueType conversion.

Uses 24 straight subdivisions for each curve and rounds to 1/1000 em.
The .otf filename contains an ordinary 0x00010000 sfnt with glyf outlines.
Output is local-only under ignored test-output/fonts; never redistribute it.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = ROOT / "test-output/fonts"
pack = struct.pack


def pad(data):
    return data + b"\0" * (-len(data) % 4)


def checksum(data):
    padded = pad(data)
    return sum(struct.unpack(">" + "I" * (len(padded) // 4), padded)) & 0xFFFFFFFF


def outline(commands):
    contours, current = [], []
    for command in commands:
        op, *v = command
        if op == "move":
            if current:
                contours.append(current)
            current = [(v[0], v[1])]
        elif op == "line":
            current.append((v[0], v[1]))
        elif op in ("quad", "curve"):
            start = current[-1]
            points = [start, *list(zip(v[::2], v[1::2]))]
            for index in range(1, 25):
                t = index / 24
                q = points
                while len(q) > 1:
                    q = [((1-t)*a[0]+t*b[0], (1-t)*a[1]+t*b[1])
                         for a, b in zip(q, q[1:])]
                current.append(q[0])
        elif op == "close":
            if current:
                contours.append(current)
                current = []
        else:
            raise ValueError(op)
    if current:
        contours.append(current)
    cleaned = []
    for contour in contours:
        values = []
        for x, y in contour:
            point = (round(x), round(y))
            if not values or point != values[-1]:
                values.append(point)
        if len(values) > 1 and values[0] == values[-1]:
            values.pop()
        if len(values) >= 3:
            cleaned.append(values)
    return cleaned


def encode_glyph(contours):
    points = [point for contour in contours for point in contour]
    if not points:
        return b"", (0, 0, 0, 0), 0, 0
    xs, ys = list(zip(*points))
    bounds = min(xs), min(ys), max(xs), max(ys)
    data = pack(">hhhhh", len(contours), *bounds)
    total = 0
    for contour in contours:
        total += len(contour)
        data += pack(">H", total - 1)
    data += pack(">H", 0)  # No hinting bytecode.
    data += bytes([1]) * len(points)  # Every sampled point is on-curve.
    for coordinate in [xs, ys]:
        previous = 0
        for value in coordinate:
            data += pack(">h", value - previous)
            previous = value
    return pad(data), bounds, len(points), len(contours)


def sfnt_tables(data):
    count = struct.unpack_from(">H", data, 4)[0]
    return {tag.decode("ascii"): (offset, length)
            for tag, _, offset, length in
            (struct.unpack_from(">4sIII", data, 12 + i * 16) for i in range(count))}


def mapped_glyph(data, codepoint):
    """Read Unicode cmap 4/12, also used to verify the generated supplementary map."""
    base = sfnt_tables(data)["cmap"][0]
    count = struct.unpack_from(">H", data, base + 2)[0]
    candidates = []
    for index in range(count):
        platform, encoding, offset = struct.unpack_from(">HHI", data, base + 4 + index * 8)
        if platform == 0 or (platform == 3 and encoding in (1, 10)):
            offset += base
            fmt = struct.unpack_from(">H", data, offset)[0]
            candidates.append((fmt, offset))
    for fmt, offset in sorted(candidates, reverse=True):
        if fmt == 12:
            groups = struct.unpack_from(">I", data, offset + 12)[0]
            for index in range(groups):
                first, last, glyph = struct.unpack_from(">III", data, offset + 16 + index * 12)
                if first <= codepoint <= last:
                    return glyph + codepoint - first
        elif fmt == 4 and codepoint < 0xFFFF:
            segments = struct.unpack_from(">H", data, offset + 6)[0] // 2
            ends = offset + 14
            starts = ends + segments * 2 + 2
            deltas = starts + segments * 2
            ranges = deltas + segments * 2
            for index in range(segments):
                first = struct.unpack_from(">H", data, starts + index * 2)[0]
                last = struct.unpack_from(">H", data, ends + index * 2)[0]
                if not first <= codepoint <= last:
                    continue
                delta = struct.unpack_from(">h", data, deltas + index * 2)[0]
                indirect = struct.unpack_from(">H", data, ranges + index * 2)[0]
                if not indirect:
                    return (codepoint + delta) & 0xFFFF
                address = ranges + index * 2 + indirect + (codepoint - first) * 2
                glyph = struct.unpack_from(">H", data, address)[0]
                return (glyph + delta) & 0xFFFF if glyph else 0
    return 0


def has_glyf_outline(data, glyph):
    if not glyph:
        return False
    tables = sfnt_tables(data)
    if not all(tag in tables for tag in ("head", "loca", "glyf", "maxp")):
        return False
    if glyph >= struct.unpack_from(">H", data, tables["maxp"][0] + 4)[0]:
        return False
    long_offsets = struct.unpack_from(">h", data, tables["head"][0] + 50)[0]
    fmt, width, factor = (">I", 4, 1) if long_offsets else (">H", 2, 2)
    start = factor * struct.unpack_from(fmt, data, tables["loca"][0] + glyph * width)[0]
    end = factor * struct.unpack_from(fmt, data, tables["loca"][0] + (glyph + 1) * width)[0]
    return end > start and struct.unpack_from(">h", data, tables["glyf"][0] + start)[0] != 0


def verified_emoji_fallbacks(deferred):
    if not deferred:
        return []
    # Match the dependency actually pinned by this workspace, never a remotely
    # fetched or unrelated system emoji font. These are egui's existing defaults.
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    match = re.search(r'name = "epaint_default_fonts"\nversion = "([^\"]+)"', lock)
    if not match:
        raise RuntimeError("No pinned egui default fonts available for outline-free emoji")
    cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
    folders = sorted((cargo_home / "registry/src").glob(f"*/epaint_default_fonts-{match[1]}/fonts"))
    if not folders:
        raise RuntimeError("Pinned egui emoji fonts are not cached; cannot verify fallback coverage")
    fonts = []
    for filename in ("NotoEmoji-Regular.ttf", "emoji-icon-font.ttf"):
        path = folders[0] / filename
        data = path.read_bytes()
        fonts.append((path, data))
    output = []
    for item in deferred:
        codepoint = item["codepoint"]
        if not item.get("isEmoji"):
            continue  # A missing Chinese/text outline is always a build failure.
        for path, data in fonts:
            glyph = mapped_glyph(data, codepoint)
            if has_glyf_outline(data, glyph):
                output.append({**item, "fallback_font": path.name,
                               "fallback_path": str(path), "fallback_glyph_id": glyph,
                               "fallback_sha256": hashlib.sha256(data).hexdigest(),
                               "rendering": "existing egui monochrome outline fallback"})
                break
    return output


def build_cmap(glyphs):
    mapping = [(item["codepoint"], index + 1) for index, item in enumerate(glyphs)]
    if mapping != sorted(mapping) or len({cp for cp, _ in mapping}) != len(mapping):
        raise ValueError("Exported codepoints must be sorted and unique")
    if len(glyphs) >= 65535:
        raise ValueError("TrueType glyph count exceeds 16-bit range")
    # Format 4 covers BMP only. U+FFFF is reserved for its sentinel segment.
    bmp = [(cp, gid) for cp, gid in mapping if cp < 0xFFFF] + [(0xFFFF, 0)]
    segs = len(bmp)
    selector = segs.bit_length() - 1
    search = 2 * (1 << selector)
    length = 16 + segs * 8
    if length > 65535:
        raise ValueError("BMP cmap 4 exceeds 64 KiB")
    table4 = pack(">HHHHHHH", 4, length, 0, segs * 2, search, selector, segs * 2 - search)
    table4 += pack(">" + "H" * segs, *(cp for cp, _ in bmp)) + pack(">H", 0)
    table4 += pack(">" + "H" * segs, *(cp for cp, _ in bmp))
    table4 += pack(">" + "H" * segs, *((gid - cp) & 0xFFFF for cp, gid in bmp))
    table4 += b"\0\0" * segs
    groups = []
    for cp, gid in mapping:
        if groups and cp == groups[-1][1] + 1 and gid == groups[-1][2] + cp - groups[-1][0]:
            groups[-1][1] = cp
        else:
            groups.append([cp, cp, gid])
    table12 = pack(">HHIII", 12, 0, 16 + len(groups) * 12, 0, len(groups))
    table12 += b"".join(pack(">III", *group) for group in groups)
    # Windows Unicode BMP and full-repertoire records, understood by ab_glyph.
    return (pack(">HH", 0, 2) + pack(">HHI", 3, 1, 20)
            + pack(">HHI", 3, 10, 20 + len(table4)) + table4 + table12)


def build_font(style, weight_class, source_characters):
    DIRECTORY.mkdir(parents=True, exist_ok=True)
    characters = source_characters | set("启动下载设置游戏我的世界 ")
    for start, end in [(0x20, 0x7F), (0x3000, 0x3040), (0xFF01, 0xFF61)]:
        characters.update(chr(code) for code in range(start, end))
    basename = f"PingFang-{style}"
    character_file = DIRECTORY / f"{basename}.characters.txt"
    character_file.write_text("".join(sorted(characters)), encoding="utf-8")
    outline_file = DIRECTORY / f"{basename}.outlines.json"
    subprocess.run(["xcrun", "swift", str(ROOT / "scripts/export-pingfang-outlines.swift"),
                    str(character_file), str(outline_file), f"PingFangSC-{style}"], check=True)
    document = json.loads(outline_file.read_text(encoding="utf-8"))
    glyphs = document["glyphs"]
    covered = {g["codepoint"] for g in glyphs}
    fallbacks = verified_emoji_fallbacks(document.get("deferredGlyphs", []))
    fallback_covered = {item["codepoint"] for item in fallbacks}
    local_missing = sorted(ord(char) for char in source_characters if ord(char) not in covered)
    missing_source = sorted(codepoint for codepoint in local_missing if codepoint not in fallback_covered)
    if missing_source:
        raise RuntimeError(f"{style} cannot cover Rust source characters: "
                           + ", ".join(f"U+{cp:04X}" for cp in missing_source))
    tables, glyf, locations, metrics = {}, b"", [0], b""
    max_points = max_contours = 0
    bounds = []
    records = [{"advance": 1000, "leftSideBearing": 0, "commands": []}, *glyphs]
    for item in records:
        data, box, count, contours = encode_glyph(outline(item["commands"]))
        glyf += data
        locations.append(len(glyf))
        max_points, max_contours = max(max_points, count), max(max_contours, contours)
        bounds.append(box)
        metrics += pack(">Hh", round(item["advance"]), round(item["leftSideBearing"]))
    count = len(records)
    global_box = (min(b[0] for b in bounds), min(b[1] for b in bounds),
                  max(b[2] for b in bounds), max(b[3] for b in bounds))
    ascent, descent = round(document["ascent"]), -round(document["descent"])
    tables['glyf'] = glyf
    tables['loca'] = pack('>'+'I'*len(locations), *locations)
    tables['hmtx'] = metrics
    # Semibold is weight 600, not a synthetic Regular with a Bold flag.
    # A future actual Bold face (700+) must set both corresponding bold flags.
    mac_style = 1 if weight_class >= 700 else 0
    selection = 0x20 if weight_class >= 700 else (0x40 if style == "Regular" else 0)
    tables['head'] = pack('>IIIIHHQQhhhhHHhhh', 0x10000, 0x10000, 0, 0x5F0F3CF5,
                          3, 1000, 0, 0, *global_box, mac_style, 8, 2, 1, 0)
    tables['hhea'] = pack('>IhhhHhhhhhhhhhhhH', 0x10000, ascent, descent, 0,
                          max(round(g['advance']) for g in records),
                          min(round(g['leftSideBearing']) for g in records),
                          0, global_box[2], 1, 0, 0, 0, 0, 0, 0, 0, count)
    tables['maxp'] = pack('>I'+'H'*14, 0x10000, count, max_points, max_contours,
                          0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0)
    tables['cmap'] = build_cmap(glyphs)
    strings, entries = b'', []
    for key, value in [(1,'PCL Local PingFang SC'),(2,style),
                       (3,f'PCLLocalPingFangSC-{style}-local-only'),
                       (4,f'PCL Local PingFang SC {style}'),(5,'Version 1.0'),
                       (6,f'PCLLocalPingFangSC-{style}'),
                       (16,'PCL Local PingFang SC'),(17,style)]:
        data=value.encode('utf-16-be')
        entries.append(pack('>HHHHHH',3,1,0x409,key,len(data),len(strings)))
        strings += data
    tables['name'] = pack('>HHH',0,len(entries),6+12*len(entries))+b''.join(entries)+strings
    tables['post'] = pack('>IihhIIIII',0x30000,0,-100,50,0,0,0,0,0)
    # OS/2 version 0 metrics; the locally derived font is not licensed for distribution.
    os2=bytearray(78)
    struct.pack_into('>HhHHH',os2,0,0,1000,weight_class,5,2)
    os2[58:62]=b'LOCL'
    struct.pack_into('>HHHhhhHH',os2,62,selection,min(0xFFFF, min(g['codepoint'] for g in glyphs)),
                     min(0xFFFF, max(g['codepoint'] for g in glyphs)),ascent,descent,0,
                     max(ascent,global_box[3]),max(-descent,-global_box[1]))
    tables['OS/2']=bytes(os2)
    tags=sorted(tables)
    n=len(tags); selector=n.bit_length()-1; search=16*(1<<selector)
    header=pack('>IHHHH',0x10000,n,search,selector,16*n-search)
    directory=b''; body=b''; offsets={}
    for tag in tags:
        data=tables[tag]; offset=12+16*n+len(body); offsets[tag]=offset
        directory+=pack('>4sIII',tag.encode(),checksum(data),offset,len(data))
        body+=pad(data)
    output=bytearray(header+directory+body)
    struct.pack_into('>I',output,offsets['head']+8,(0xB1B0AFBA-checksum(output))&0xFFFFFFFF)
    assert checksum(output)==0xB1B0AFBA
    for item in glyphs:
        codepoint = item["codepoint"]
        glyph = mapped_glyph(output, codepoint)
        if not glyph or (item["commands"] and not has_glyf_outline(output, glyph)):
            raise RuntimeError(f"Generated cmap/outline missing U+{codepoint:04X}")
    for item in fallbacks:
        if mapped_glyph(output, item["codepoint"]):
            raise RuntimeError("Outline-free emoji must not shadow the fallback font")
    path=DIRECTORY/f'{basename}.otf'
    temporary = path.with_suffix('.temporary.otf')
    temporary.write_bytes(output)
    temporary.replace(path)
    source={"source_font":document['font'],"source_path":document['source'],
            "format":"sfnt TrueType glyf; Unicode cmap 4 + 12; curve outlines sampled at 24 subdivisions",
            "style":style,"os2_weight_class":weight_class,
            "os2_fs_selection":selection,"head_mac_style":mac_style,
            "rust_source_character_count":len(source_characters),
            "source_text_scope":"Rust UI source, bundled help/catalog.json, and CJK ideographs in the frozen wiki index",
            "missing_rust_source_codepoints":missing_source,
            "local_font_omitted_source_codepoints":local_missing,
            "egui_fallback_glyphs":fallbacks,
            "deferred_without_outline":document.get('deferredGlyphs', []),
            "intentional_blank_codepoints":[g['codepoint'] for g in glyphs if g.get('intentionalBlank')],
            "missing_extra_codepoints":document['missing'],
            "fallback_glyphs":document['fallbackGlyphs'],
            "glyph_count":len(glyphs),"output":str(path),
            "output_sha256":hashlib.sha256(output).hexdigest(),
            "scope":"Local-only; do not publish, commit, or redistribute."}
    path.with_suffix('.source.json').write_text(json.dumps(source,ensure_ascii=False,indent=2)+'\n', encoding="utf-8")
    print(json.dumps({key: source[key] for key in ["source_font", "style", "os2_weight_class",
                     "glyph_count", "missing_rust_source_codepoints", "local_font_omitted_source_codepoints", "output", "output_sha256"]},
                     ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--style", choices=["regular", "semibold", "all"], default="regular",
                        help="default preserves the original Regular-only command")
    args = parser.parse_args()
    source_characters = set()
    for source in (ROOT / "crates").rglob("*.rs"):
        source_characters.update(source.read_text(encoding="utf-8"))
    # Bundled help text is rendered like Rust UI labels and needs the same
    # local PingFang subset. Do not collect downloaded/user files.
    help_catalog = ROOT / "crates/pcl-desktop/assets/help/catalog.json"
    if help_catalog.is_file():
        source_characters.update(help_catalog.read_text(encoding="utf-8"))
    wiki_index = ROOT / "crates/pcl-core/assets/wiki/WikiEntries.txt"
    if wiki_index.is_file():
        # Catalog titles are external project names and may contain invisible
        # shaping controls or color-only emoji. Extend the Chinese subset;
        # ordinary Latin/emoji still use egui's existing fallback fonts.
        source_characters.update(char for char in wiki_index.read_text(encoding="utf-8")
                                 if 0x3400 <= ord(char) <= 0x9FFF
                                 or 0xF900 <= ord(char) <= 0xFAFF
                                 or 0x20000 <= ord(char) <= 0x323AF)
    source_characters = {char for char in source_characters if ord(char) >= 32 and ord(char) != 127}
    for style, weight_class in [("Regular", 400), ("Semibold", 600)]:
        if args.style in ("all", style.lower()):
            build_font(style, weight_class, source_characters)


if __name__=='__main__':
    main()
