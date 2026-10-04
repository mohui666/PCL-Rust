// Exports outlines through the locally installed Apple CoreText font renderer.
// The generated font data is local-only; never publish it with application assets.
import Foundation
import CoreText
import CoreGraphics

guard (3...4).contains(CommandLine.arguments.count) else {
    fputs("usage: swift export-pingfang-outlines.swift characters.txt outlines.json [PingFangSC-Regular|PingFangSC-Semibold]\n", stderr)
    exit(2)
}
let input = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
let requestedName = CommandLine.arguments.count == 4 ? CommandLine.arguments[3] : "PingFangSC-Regular"
guard ["PingFangSC-Regular", "PingFangSC-Semibold"].contains(requestedName) else {
    fatalError("Unsupported local font face: \(requestedName)")
}
let font = CTFontCreateWithName(requestedName as CFString, 1000, nil)
let postScriptName = CTFontCopyPostScriptName(font) as String
guard postScriptName == requestedName else {
    fatalError("CoreText did not resolve \(requestedName): \(postScriptName)")
}
var glyphs: [[String: Any]] = []
var missing: [UInt32] = []
var fallbackGlyphs: [[String: Any]] = []
var deferredGlyphs: [[String: Any]] = []
// CoreText consumes UTF-16, including both code units of supplementary scalars.
// A surrogate pair maps to one glyph plus an empty slot; never map a half pair.
func mappedGlyph(_ scalar: UnicodeScalar, _ font: CTFont) -> CGGlyph? {
    let units = Array(String(scalar).utf16)
    var result = [CGGlyph](repeating: 0, count: units.count)
    units.withUnsafeBufferPointer { characters in
        result.withUnsafeMutableBufferPointer { glyphs in
            _ = CTFontGetGlyphsForCharacters(font, characters.baseAddress!, glyphs.baseAddress!, units.count)
        }
    }
    let mapped = result.filter { $0 != 0 }
    return mapped.count == 1 ? mapped[0] : nil
}
for scalar in Set(input.unicodeScalars.map { $0.value }).sorted() {
    guard let value = UnicodeScalar(scalar) else { continue }
    let units = Array(String(value).utf16)
    var glyphFont = font
    var glyph = mappedGlyph(value, glyphFont)
    var fallbackRecord: [String: Any]? = nil
    if glyph == nil {
        glyphFont = CTFontCreateForString(font, String(value) as CFString, CFRange(location: 0, length: units.count))
        glyph = mappedGlyph(value, glyphFont)
        let fallbackSource = CTFontCopyAttribute(glyphFont, kCTFontURLAttribute) as? URL
        fallbackRecord = ["codepoint": scalar,
                          "font": CTFontCopyPostScriptName(glyphFont) as String,
                          "source": fallbackSource?.path ?? "unknown"]
    }
    guard var glyph = glyph else { missing.append(scalar); continue }
    var advance = CGSize.zero
    CTFontGetAdvancesForGlyphs(glyphFont, .horizontal, &glyph, &advance, 1)
    var bounds = CGRect.zero
    CTFontGetBoundingRectsForGlyphs(glyphFont, .horizontal, &glyph, &bounds, 1)
    var commands: [[Any]] = []
    if let path = CTFontCreatePathForGlyph(glyphFont, glyph, nil) {
        path.applyWithBlock { pointer in
            let item = pointer.pointee
            switch item.type {
            case .moveToPoint:
                commands.append(["move", item.points[0].x, item.points[0].y])
            case .addLineToPoint:
                commands.append(["line", item.points[0].x, item.points[0].y])
            case .addQuadCurveToPoint:
                commands.append(["quad", item.points[0].x, item.points[0].y, item.points[1].x, item.points[1].y])
            case .addCurveToPoint:
                commands.append(["curve", item.points[0].x, item.points[0].y, item.points[1].x, item.points[1].y, item.points[2].x, item.points[2].y])
            case .closeSubpath:
                commands.append(["close"])
            @unknown default:
                fatalError("Unknown CoreGraphics path operation")
            }
        }
    }
    // The catalog includes U+3164 HANGUL FILLER in an attribution name.
    // Its intentional blank advance is different from an unsupported emoji.
    let intentionalBlank = CharacterSet.whitespacesAndNewlines.contains(value) || scalar == 0x3164
    if commands.isEmpty && !intentionalBlank {
        // AppleColorEmoji has bitmap/color glyphs, not glyf-compatible paths.
        // A cmap entry with an empty outline would block egui's real emoji font.
        let source = CTFontCopyAttribute(glyphFont, kCTFontURLAttribute) as? URL
        deferredGlyphs.append(["codepoint": scalar,
                               "font": CTFontCopyPostScriptName(glyphFont) as String,
                               "source": source?.path ?? "unknown",
                               "isEmoji": value.properties.isEmoji,
                               "reason": "CoreText glyph has no vector outline; omitted from cmap"])
        continue
    }
    if let fallback = fallbackRecord { fallbackGlyphs.append(fallback) }
    glyphs.append(["codepoint": scalar, "advance": advance.width,
                   "leftSideBearing": bounds.minX, "commands": commands,
                   "intentionalBlank": commands.isEmpty && intentionalBlank])
}
let sourceURL = CTFontCopyAttribute(font, kCTFontURLAttribute) as? URL
let result: [String: Any] = [
    "font": postScriptName,
    "source": sourceURL?.path ?? "unknown",
    "unitsPerEm": 1000,
    "ascent": CTFontGetAscent(font),
    "descent": CTFontGetDescent(font),
    "leading": CTFontGetLeading(font),
    "glyphs": glyphs,
    "missing": missing,
    "fallbackGlyphs": fallbackGlyphs,
    "deferredGlyphs": deferredGlyphs,
]
try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
    .write(to: URL(fileURLWithPath: CommandLine.arguments[2]), options: .atomic)
print("CoreText exported \(glyphs.count) glyphs from \(postScriptName); missing \(missing.count); deferred without outlines \(deferredGlyphs.count)")
