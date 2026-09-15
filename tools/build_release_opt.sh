#!/bin/bash
# build_release_opt.sh — bench 专用优化构建: PGO + panic=abort 按平台默认。
#
# 决策全部内聚于此 (drone 命令行保持单条无逻辑, 兼容 Windows exec runner
# 的启动期解析, 记忆 #148):
#   - Linux/macOS: PGO (tools/build_pgo.sh) + -Cpanic=abort + lua_longjmp。
#     CI #120 实证: 浮点 1.1182→1.0318 (1.54→1.45x), 整数 -47%,
#     all.lua 1.96→1.65s, 12 项零回退。abort 移除 execute_loop 全部
#     unwind landing pads; C 模块错误边界由 lua_longjmp (setjmp/longjmp,
#     size-opt 同源实战) 承接。
#   - Windows: PGO 但不带 abort — MSVC+PGO 对该 env 组合敏感 (#118
#     probe_cache 同型回归), 保守保持现状。
#   - BENCH_ABORT=0 / BENCH_PGO=0: 显式关闭对应层 (代码改动轮次的纯 A/B)。
#   - 工具链缺失/任一步失败: build_pgo.sh 内部 PGO_FALLBACK 回退普通构建。
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

if [ "${BENCH_PGO:-1}" = "0" ]; then
    if [ "${BENCH_ABORT:-1}" != "0" ] && ! uname -s | grep -qE '^(MINGW|MSYS|CYGWIN|Windows)'; then
        RUSTFLAGS="-Cpanic=abort" cargo build --release --features lua_longjmp
    else
        cargo build --release
    fi
    exit $?
fi
if uname -s | grep -qE '^(MINGW|MSYS|CYGWIN|Windows)'; then
    # Windows: PGO only (不带 abort — MSVC+PGO 对 panic=abort+longjmp 组合
    # 未验证, #118 型 codegen 回归风险; 保持 #117 已验证形态)
    bash tools/build_pgo.sh
else
    if [ "${BENCH_ABORT:-1}" = "0" ]; then
        bash tools/build_pgo.sh
    else
        PGO_RUSTFLAGS="-Cpanic=abort" PGO_FEATURES="lua_longjmp" bash tools/build_pgo.sh
    fi
fi
