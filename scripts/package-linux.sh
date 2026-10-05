#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  printf '%s\n' 'Run this script on Linux x86_64.' >&2
  exit 1
fi
PCL_CARGO_BIN="${CARGO_BIN:-cargo}"
PCL_PYTHON_BIN="${PYTHON_BIN:-python3}"
PCL_PACKAGE="$PWD/dist/PCL-Rust-Linux-x86_64"
PCL_TARGET='x86_64-unknown-linux-gnu'
"$PCL_PYTHON_BIN" scripts/prepare-linux-fonts.py
"$PCL_CARGO_BIN" build --release --locked -p pcl-desktop --target "$PCL_TARGET"
mkdir -p "$PCL_PACKAGE/resources"
cp "target/$PCL_TARGET/release/pcl-desktop" "$PCL_PACKAGE/PCL-Rust"
chmod +x "$PCL_PACKAGE/PCL-Rust"
cp test-output/fonts/linux/NotoSansSC-Regular.ttf test-output/fonts/linux/NotoSansSC-Semibold.ttf \
   test-output/fonts/linux/OFL.txt test-output/fonts/linux/FONT-SOURCES.json "$PCL_PACKAGE/resources/"
cp UPSTREAM-LICENCE "$PCL_PACKAGE/UPSTREAM-LICENCE"
cp crates/pcl-desktop/assets/icon.png "$PCL_PACKAGE/resources/icon.png"
"$PCL_PYTHON_BIN" scripts/prepare-linux-fonts.py --verify-only --output-dir "$PCL_PACKAGE/resources"
file "$PCL_PACKAGE/PCL-Rust" | tee "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
file "$PCL_PACKAGE/PCL-Rust" | grep -q 'ELF 64-bit LSB.*x86-64'
readelf -h "$PCL_PACKAGE/PCL-Rust" >> "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
readelf -d "$PCL_PACKAGE/PCL-Rust" >> "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
readelf --version-info "$PCL_PACKAGE/PCL-Rust" >> "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
ldd --version >> "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
ldd "$PCL_PACKAGE/PCL-Rust" >> "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"
if grep -q 'not found' "$PCL_PACKAGE/ELF-DEPENDENCIES.txt"; then
  cat "$PCL_PACKAGE/ELF-DEPENDENCIES.txt" >&2
  exit 1
fi
"$PCL_PACKAGE/PCL-Rust" --help > "$PCL_PACKAGE/CLI-HELP.txt"
cat > "$PCL_PACKAGE/README.txt" <<'TXT'
PCL Rust — Linux x86_64

Run ./PCL-Rust from a Linux desktop session (X11 or Wayland).
Keep the resources directory next to the executable for Chinese text.

Runtime: glibc, libX11, libXcursor, libXrandr, libXi, libxkbcommon,
libEGL/libGL and an X11 or Wayland display. File dialogs use the
xdg-desktop-portal service supplied by the desktop environment.
Linked libraries are recorded in ELF-DEPENDENCIES.txt; graphics and
portal libraries may be loaded dynamically and are not all listed by ldd.

Chinese fonts: Noto Sans SC, SIL OFL 1.1; see resources/OFL.txt and
resources/FONT-SOURCES.json. Original PCL license: UPSTREAM-LICENCE.

The build verifies ELF format, linked libraries, CLI startup and Chinese
font outlines. It does not verify a Linux GUI session or a running game.
TXT
(
  cd "$PCL_PACKAGE"
  sha256sum PCL-Rust resources/NotoSansSC-Regular.ttf resources/NotoSansSC-Semibold.ttf \
    resources/OFL.txt resources/FONT-SOURCES.json > SHA256SUMS
)
tar -C dist -czf dist/PCL-Rust-Linux-x86_64.tar.gz PCL-Rust-Linux-x86_64
sha256sum dist/PCL-Rust-Linux-x86_64.tar.gz
printf '%s\n' "$PCL_PACKAGE"
