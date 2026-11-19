#!/bin/bash
# Regenerate the README screenshots. Requires an existing release build.
# Usage: bash scripts/screenshots.sh
set -u
cd "$(dirname "$0")/.." || exit 1
EXE=./target/release/kestrel.exe
ROOT='D:\LIIXIANG\vscode\OpenSourceProject\kestrel'
OUT=docs/screenshots
rm -rf "$APPDATA/Kestrel"
mkdir -p "$OUT"
shot() {
  local name=$1; shift
  rm -f "$OUT/$name.png" "$OUT/$name.ppm"
  env "$@" KESTREL_SHOT="$ROOT/$OUT/$name.ppm" KESTREL_SHOT_DELAY=150 KESTREL_SHOT_EXIT=1 \
    "$EXE" > "/tmp/kestrel_shot_$name.log" 2>&1 || true
  python - "$OUT/$name.ppm" "$OUT/$name.png" <<'PY'
import sys
from PIL import Image
im = Image.open(sys.argv[1])
im.save(sys.argv[2])
print('wrote', sys.argv[2], im.size)
PY
  rm -f "$OUT/$name.ppm"
}
shot editor KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs"
shot palette KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=palette KESTREL_DEV_QUERY=打开
shot quickopen KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=quickopen KESTREL_DEV_QUERY=view
shot find KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=replace KESTREL_DEV_QUERY=Theme
shot search KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=search
shot terminal KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=terminal
shot settings KESTREL_THEME=dark KESTREL_OPEN="$ROOT/src/app.rs" KESTREL_UI=settings
shot light KESTREL_THEME=light KESTREL_OPEN="$ROOT/src/app.rs"
shot welcome KESTREL_THEME=dark KESTREL_UI=welcome
