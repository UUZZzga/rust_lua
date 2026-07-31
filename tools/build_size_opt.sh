#!/bin/bash
# 体积优先构建脚本: cargo build + 后处理 strip
#
# 用法: bash tools/build_size_opt.sh
#
# 自动检测 nightly 工具链:
#   - 有 nightly: 用 panic=immediate-abort + build-std, 彻底移除 backtrace 代码 (~150KB)
#   - 无 nightly: 用 stable panic=abort (backtrace 代码保留, 二进制稍大)
#
# 后处理移除不需要的段:
#   .eh_frame (~121KB)        — CFI unwind 表 (panic=abort 不需要)
#   .gcc_except_table (~5KB)  — C++ 异常处理表
#   .comment (~<1KB)          — 编译器标识字符串
#   .note.ABI-tag             — ABI 标记
#   .note.gnu.build-id        — Build ID

set -e

cd "$(dirname "$0")/.."

CARGO_MANIFEST="$(pwd)/Cargo.toml"

# 检测 nightly 工具链
HAS_NIGHTLY=false
if rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    HAS_NIGHTLY=true
fi

if [ "$HAS_NIGHTLY" = "true" ]; then
    echo "==> nightly detected: using panic=immediate-abort + build-std"

    # 临时注入 cargo-features 和 size-opt-nightly profile
    # (stable cargo 不识别 panic=immediate-abort, 必须动态注入)
    cp "$CARGO_MANIFEST" "$CARGO_MANIFEST.bak"
    # 确保在任何情况下都恢复 Cargo.toml
    trap 'mv -f "$CARGO_MANIFEST.bak" "$CARGO_MANIFEST" 2>/dev/null' EXIT

    # 1. 在文件开头添加 cargo-features
    # 2. 在末尾添加 size-opt-nightly profile
    {
        echo 'cargo-features = ["panic-immediate-abort"]'
        echo ""
        cat "$CARGO_MANIFEST"
        echo ""
        echo '[profile.size-opt-nightly]'
        echo 'inherits = "size-opt"'
        echo 'panic = "immediate-abort"'
    } > "$CARGO_MANIFEST.tmp"
    mv "$CARGO_MANIFEST.tmp" "$CARGO_MANIFEST"

    # 用 nightly + build-std 构建 (需绕过内存限制)
    # -Z build-std-features= : 禁用 std 默认 features (backtrace 等), 移除 gimli/addr2line/miniz_oxide
    systemd-run --user --wait --collect --pipe --property=LimitAS=infinity --working-directory="$(pwd)" \
        cargo +nightly build --profile size-opt-nightly -Z build-std=std,panic_abort,core,alloc -Z build-std-features=

    # 恢复原 Cargo.toml
    mv "$CARGO_MANIFEST.bak" "$CARGO_MANIFEST"
    trap - EXIT

    BINARY="target/size-opt-nightly/lua"
else
    echo "==> stable: using panic=abort (no build-std)"

    systemd-run --user --wait --collect --pipe --property=LimitAS=infinity --working-directory="$(pwd)" \
        cargo build --profile size-opt

    BINARY="target/size-opt/lua"
fi

echo "==> strip debug + unnecessary sections"
# --strip-all: 移除所有符号和调试信息
# copy to temp then rename (objcopy can't write to same file)
objcopy --strip-all \
    --remove-section=.eh_frame \
    --remove-section=.gcc_except_table \
    --remove-section=.comment \
    --remove-section=.note.ABI-tag \
    --remove-section=.note.gnu.build-id \
    "$BINARY" "$BINARY.stripped"
mv "$BINARY.stripped" "$BINARY"

echo "==> 完成"
ls -la "$BINARY"
