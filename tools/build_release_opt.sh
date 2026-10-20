#!/bin/bash
# build_release_opt.sh — bench 专用优化构建: panic=abort + lua_longjmp (PGO 可选)。
#
# 决策全部内聚于此 (drone 命令行保持单条无逻辑, 兼容 Windows exec runner
# 的启动期解析, 记忆 #148):
#   - 全平台默认: -Cpanic=abort + lua_longjmp, PGO 关闭 (需 BENCH_PGO=1 开启)。
#     abort 移除 execute_loop 全部 unwind landing pads; C 模块错误边界由
#     lua_longjmp (setjmp/longjmp, size-opt 同源实战) 承接。
#   - PGO 开启 (BENCH_PGO=1, tools/build_pgo.sh) 时的历史实测保留备查:
#     Linux CI #120: 浮点 1.54→1.45x, 整数 -47%, all.lua 1.96→1.65s。
#     Windows 本地 PGO A/B: float -3.5~5%, calls -7~9%, int -4~12%,
#     错误套件全过。
#   - BENCH_ABORT=0 / BENCH_PGO=1: 显式切换对应层 (代码改动轮次的纯 A/B)。
#   - 工具链缺失/任一步失败 (PGO 开启时): build_pgo.sh 内部 PGO_FALLBACK 回退普通构建。
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

ABORT=1
[ "${BENCH_ABORT:-1}" = "0" ] && ABORT=0

if [ "${BENCH_PGO:-0}" = "0" ]; then
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
