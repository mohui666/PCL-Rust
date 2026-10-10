#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
CARGO_BIN="${CARGO_BIN:-$HOME/.cargo/bin/cargo}"
PCL_MACOS_FONT_MODE="${PCL_MACOS_FONT_MODE:-local}"
case "$PCL_MACOS_FONT_MODE" in
  local) python3 scripts/build-local-pingfang-sfnt.py --style all ;;
  noto) "${PYTHON_BIN:-python3}" scripts/prepare-linux-fonts.py ;;
  *) printf 'Unknown PCL_MACOS_FONT_MODE: %s\n' "$PCL_MACOS_FONT_MODE" >&2; exit 1 ;;
esac
"$CARGO_BIN" build --release --locked -p pcl-desktop -p pcl-cli
PCL_BUNDLE="${PCL_BUNDLE_DIR:-$PWD/dist/PCL Rust.app}"
mkdir -p "$PCL_BUNDLE/Contents/MacOS" "$PCL_BUNDLE/Contents/Resources"
cp target/release/pcl-desktop "$PCL_BUNDLE/Contents/MacOS/pcl-desktop"
cp target/release/pcl-cli "$PCL_BUNDLE/Contents/MacOS/pcl-cli"
if [[ "$PCL_MACOS_FONT_MODE" == noto ]]; then
  for PCL_FONT_STYLE in Regular Semibold; do
    cp "test-output/fonts/linux/NotoSansSC-$PCL_FONT_STYLE.ttf" "$PCL_BUNDLE/Contents/Resources/"
    rm -f "$PCL_BUNDLE/Contents/Resources/PingFang-$PCL_FONT_STYLE.otf"
  done
  cp test-output/fonts/linux/OFL.txt test-output/fonts/linux/FONT-SOURCES.json "$PCL_BUNDLE/Contents/Resources/"
else
  for PCL_FONT_STYLE in Regular Semibold; do
    cp "test-output/fonts/PingFang-$PCL_FONT_STYLE.otf" "$PCL_BUNDLE/Contents/Resources/"
    rm -f "$PCL_BUNDLE/Contents/Resources/NotoSansSC-$PCL_FONT_STYLE.ttf"
  done
  rm -f "$PCL_BUNDLE/Contents/Resources/OFL.txt" "$PCL_BUNDLE/Contents/Resources/FONT-SOURCES.json"
fi
cp UPSTREAM-LICENCE "$PCL_BUNDLE/Contents/Resources/UPSTREAM-LICENCE"
cp crates/pcl-desktop/assets/icon.icns "$PCL_BUNDLE/Contents/Resources/PCL-Rust.icns"
PCL_NOTICES="$PCL_BUNDLE/Contents/Resources/licenses"
mkdir -p "$PCL_NOTICES"
cp crates/pcl-core/assets/launch/JavaWrapper-LICENCE crates/pcl-core/assets/launch/LwjglUnsafeAgent-LICENSE "$PCL_NOTICES/"
cp crates/pcl-core/assets/launch/SOURCES.md "$PCL_NOTICES/LAUNCH-PATCH-SOURCES.md"
cp crates/pcl-desktop/assets/upstream/README-SOURCES.md "$PCL_NOTICES/UPSTREAM-RESOURCES.md"
cp crates/pcl-desktop/assets/upstream/SOURCE-MANIFEST.json "$PCL_NOTICES/UPSTREAM-SOURCE-MANIFEST.json"
cp crates/pcl-desktop/assets/help/README.md "$PCL_NOTICES/HELP-RESOURCES.md"
cp crates/pcl-desktop/assets/help/SOURCE.json "$PCL_NOTICES/HELP-SOURCE.json"
cp crates/pcl-core/assets/wiki/SOURCES.md "$PCL_NOTICES/WIKI-SOURCES.md"
cp docs/assets/README.md "$PCL_NOTICES/ICON-SOURCES.md"
cat > "$PCL_BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>pcl-desktop</string>
<key>CFBundleIdentifier</key><string>local.pcl-rust.thirdparty</string>
<key>CFBundleName</key><string>PCL Rust</string>
<key>CFBundleDisplayName</key><string>PCL Rust</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleIconFile</key><string>PCL-Rust.icns</string>
<key>CFBundleShortVersionString</key><string>0.1.1</string>
<key>CFBundleVersion</key><string>2</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSHumanReadableCopyright</key><string>原作者：龙腾猫跃。本应用为第三方 Rust 实验重构版，非官方产品。</string>
</dict></plist>
PLIST
codesign --force --sign - "$PCL_BUNDLE"
codesign --verify --deep --strict "$PCL_BUNDLE"
printf '%s\n' "$PCL_BUNDLE"
