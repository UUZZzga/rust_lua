#!/bin/bash
# build_release_opt.sh — bench 专用优化构建: PGO + panic=abort + lua_longjmp。
#
# 决策全部内聚于此 (drone 命令行保持单条无逻辑, 兼容 Windows exec runner
# 的启动期解析, 记忆 #148):
#   - 全平台默认: PGO (tools/build_pgo.sh) + -Cpanic=abort + lua_longjmp。
#     Linux CI #120 实证: 浮点 1.54→1.45x, 整数 -47%, all.lua 1.96→1.65s。
#     abort 移除 execute_loop 全部 unwind landing pads; C 模块错误边界由
#     lua_longjmp (setjmp/longjmp, size-opt 同源实战) 承接。
#     Windows 本地 PGO A/B: float -3.5~5%, calls -7~9%, int -4~12%,
#     错误套件全过 — 原"MSVC 保守不带"是纯推理保留 (与 #120 前对 abort
#     的推理否决同类), 现统一启用, CI 为最终裁判。
#   - BENCH_ABORT=0 / BENCH_PGO=0: 显式关闭对应层 (代码改动轮次的纯 A/B)。
#   - 工具链缺失/任一步失败: build_pgo.sh 内部 PGO_FALLBACK 回退普通构建。
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

ABORT=1
[ "${BENCH_ABORT:-1}" = "0" ] && ABORT=0

if [ "${BENCH_PGO:-1}" = "0" ]; then
    if [ "$ABORT" = "1" ]; then
        RUSTFLAGS="-Cpanic=abort" cargo build --release --features lua_longjmp
    else
        cargo build --release
    fi
    exit $?
fi

if [ "$ABORT" = "1" ]; then
    PGO_RUSTFLAGS="-Cpanic=abort" PGO_FEATURES="lua_longjmp" bash tools/build_pgo.sh
else
    bash tools/build_pgo.sh
fi
