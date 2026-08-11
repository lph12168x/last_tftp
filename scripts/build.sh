#!/usr/bin/env bash
# last-tftp 跨平台构建脚本
#
# 用法:
#   ./scripts/build.sh linux       # 当前 Linux 平台
#   ./scripts/build.sh windows     # 交叉编译 Windows (需 mingw)
#   ./scripts/build.sh macos       # 交叉编译 macOS (需 osxcross)
#   ./scripts/build.sh all         # 全平台
#
# 输出在 target/<triple>/release/last-tftp[.exe]

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

target() {
    local platform="$1"
    case "$platform" in
        linux)
            cargo build --release
            echo "built: target/release/last-tftp"
            ;;
        windows)
            # 需要 rustup target add x86_64-pc-windows-gnu
            # 还需要 mingw: apt install gcc-mingw-w64-x86-64
            rustup target add x86_64-pc-windows-gnu 2>/dev/null || true
            cargo build --release --target x86_64-pc-windows-gnu
            echo "built: target/x86_64-pc-windows-gnu/release/last-tftp.exe"
            ;;
        macos)
            # 需要 osxcross + 工具链
            rustup target add x86_64-apple-darwin 2>/dev/null || true
            cargo build --release --target x86_64-apple-darwin
            echo "built: target/x86_64-apple-darwin/release/last-tftp"
            ;;
        all)
            "$0" linux
            "$0" windows 2>/dev/null || echo "WARN: windows cross-compile skipped (needs mingw)"
            "$0" macos 2>/dev/null || echo "WARN: macos cross-compile skipped (needs osxcross)"
            ;;
        *)
            echo "usage: $0 {linux|windows|macos|all}" >&2
            exit 1
            ;;
    esac
}

target "${1:-linux}"