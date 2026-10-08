#!/usr/bin/env bash
# Package the Windows release into a shareable zip:
#   dist/pound-<version>-win64.zip
#   └─ pound-<version>-win64/ { pound.exe, QUICK-START.txt, LICENSE }
#
# Cross-compiles from Linux (needs the x86_64-pc-windows-gnu rust target and
# mingw-w64). CI runs this on v* tags; recipients follow QUICK-START.txt.
# Expect SmartScreen to warn on the unsigned exe ("More info" > "Run anyway").
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')
TARGET=x86_64-pc-windows-gnu
TARGET_DIR="target/$TARGET/release"
STAGE="dist/pound-$VERSION-win64"

echo "==> Building pound $VERSION for $TARGET"
cargo build --release --target "$TARGET"

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$TARGET_DIR/pound.exe" "$STAGE/"
cp resources/quick-start.txt "$STAGE/QUICK-START.txt"
cp LICENSE "$STAGE/"

# No `zip` in a default WSL install; python3's zipfile is always there.
# (Concatenate the name — with_suffix would mangle the dotted version.)
rm -f "$STAGE.zip"
python3 - "$STAGE" <<'EOF'
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
