#!/usr/bin/env bash
# Package the Windows release into a shareable zip:
#   dist/pound-<version>-win64.zip
#   └─ pound-<version>-win64/ { pound.exe, QUICK-START.txt, LICENSE }
#
# CI (release.yml) runs this on a windows-latest runner with the MSVC
# toolchain, which links WebView2Loader STATICALLY. Do not build releases
# with windows-gnu/mingw: it imports WebView2Loader.dll dynamically and the
# app dies at startup with STATUS_DLL_NOT_FOUND on user machines.
#
# Locally on Windows (VS Build Tools + rustup msvc):
#   ./scripts/package.sh
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="${1:-x86_64-pc-windows-msvc}"
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
TARGET_DIR="target/$TARGET/release"
STAGE="dist/pound-$VERSION-win64"

echo "==> Building pound $VERSION for $TARGET"
cargo build --release --target "$TARGET"

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$TARGET_DIR/pound.exe" "$STAGE/"
cp resources/quick-start.txt "$STAGE/QUICK-START.txt"
cp LICENSE "$STAGE/"

# `zip` is not everywhere; python's zipfile is. Windows runners expose
# `python`, Unix hosts `python3`.
PY="$(command -v python3 || command -v python)"
rm -f "$STAGE.zip"
"$PY" - "$STAGE" <<'EOF'
import pathlib, sys, zipfile

stage = pathlib.Path(sys.argv[1])
out = stage.parent / f"{stage.name}.zip"
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
    for p in sorted(stage.rglob("*")):
        if p.is_file():
            zf.write(p, p.relative_to(stage.parent))
EOF
rm -rf "$STAGE"

echo
echo "Done: dist/pound-$VERSION-win64.zip — pushing a v* tag lets"
echo ".github/workflows/release.yml attach it to a GitHub Release."
