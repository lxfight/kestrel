#!/bin/bash
# 生成 README 截图与审计截图矩阵。
# 用法：
#   bash scripts/screenshots.sh                  # 默认输出到 docs/screenshots
#   OUT=docs/screenshots/current bash scripts/screenshots.sh
#   EXE=target/debug/kestrel.exe bash scripts/screenshots.sh
# 可选环境变量：EXE、OUT、PY、KESTREL_SHOT_DELAY
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# MSYS 的 /d/... 形式 Windows 程序不认，转成 D:\... 再传给应用
ROOT_WIN="$(cygpath -w "$ROOT" 2>/dev/null || printf '%s' "$ROOT")"
cd "$ROOT" || exit 1

# 默认用最新的构建产物：release 存在且不比 debug 旧才用它
if [ -z "${EXE:-}" ]; then
  if [ -x target/debug/kestrel.exe ] && [ ! target/release/kestrel.exe -nt target/debug/kestrel.exe ]; then
    EXE=target/debug/kestrel.exe
  else
    EXE=target/release/kestrel.exe
  fi
fi
OUT="${OUT:-docs/screenshots}"
PY="${PY:-python}"
DELAY="${KESTREL_SHOT_DELAY:-150}"

# 用临时 APPDATA，避免动用户真实设置与会话
SBOX="$(mktemp -d)"
export APPDATA="$(cygpath -w "$SBOX" 2>/dev/null || printf '%s' "$SBOX")"
mkdir -p "$OUT"

shot() {
  local name="$1"; shift
  local ppm="$ROOT/$OUT/$name.ppm"
  local ppm_win="$ROOT_WIN/$OUT/$name.ppm"
  rm -f "$ppm" "$OUT/$name.png"
  env "$@" KESTREL_SHOT="$ppm_win" KESTREL_SHOT_DELAY="$DELAY" KESTREL_SHOT_EXIT=1 \
    "$EXE" > "$SBOX/$name.log" 2>&1 &
  local pid=$!
  local i
  for i in $(seq 1 160); do
    [ -f "$ppm" ] && break
    sleep 0.25
  done
  sleep 0.4
  kill -9 "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  if [ ! -f "$ppm" ]; then echo "FAIL $name (see $SBOX/$name.log)"; return 1; fi
  "$PY" - "$ppm_win" "$ROOT_WIN/$OUT/$name.png" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
  rm -f "$ppm"
  echo "OK $name"
}

SRC="$ROOT_WIN/src/app.rs"
ROOT_DIR="$ROOT_WIN"

# README 主图
shot editor      KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC"
shot palette     KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=palette   KESTREL_DEV_QUERY=打开
shot quickopen   KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=quickopen KESTREL_DEV_QUERY=view
shot find        KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=replace   KESTREL_DEV_QUERY=Theme
shot search      KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=search
shot terminal    KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=terminal
shot settings    KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=settings
shot light       KESTREL_THEME=light KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC"
shot welcome     KESTREL_THEME=dark

# 审计矩阵
shot light-welcome  KESTREL_THEME=light
shot git            KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=git
shot outline        KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=outline
shot problems       KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=problems
shot plugins        KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=plugins
shot shortcuts      KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=shortcuts
shot split          KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=split
shot md-split       KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$ROOT_WIN/README.md" KESTREL_UI=md_split
shot editor-small   KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_WINDOW=760x480
shot palette-small  KESTREL_THEME=dark  KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=palette KESTREL_WINDOW=1024x700
shot terminal-light KESTREL_THEME=light KESTREL_OPEN="$ROOT_DIR" KESTREL_DEV_OPEN="$SRC" KESTREL_UI=terminal

rm -rf "$SBOX"
echo "done -> $OUT"
