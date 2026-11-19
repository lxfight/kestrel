#!/bin/bash
# 构建 Kestrel 并把产物放到仓库根的 Kestrel.exe。
# 用法：
#   bash scripts/build.sh            # release 构建 + 复制到 ./Kestrel.exe
#   bash scripts/build.sh --debug    # debug 构建（产物 target/debug/kestrel.exe）
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# 本机 cargo 不在默认 PATH 时的兜底（工作机环境）
if ! command -v cargo >/dev/null 2>&1; then
  export CARGO_HOME="${CARGO_HOME:-D:/DevCache/cargo}"
  export RUSTUP_HOME="${RUSTUP_HOME:-D:/DevCache/rustup}"
  export PATH="D:/DevCache/rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
fi

PROFILE=release
if [ "${1:-}" = "--debug" ]; then PROFILE=debug; fi

echo "== cargo build ($PROFILE)"
cargo build --profile "$PROFILE"

if [ "$PROFILE" = "release" ]; then
  cp -f target/release/kestrel.exe "$ROOT/Kestrel.exe"
  ls -la "$ROOT/Kestrel.exe"
  echo "== done -> Kestrel.exe"
else
  ls -la target/debug/kestrel.exe
fi
