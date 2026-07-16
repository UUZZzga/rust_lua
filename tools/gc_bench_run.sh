#!/bin/bash
# gc_bench_run.sh — 运行 GC 性能测试并对比 C 实现与 Rust 实现
#
# 用法:
#   bash tools/gc_bench_run.sh [选项] [N] [ROUNDS] [STRIDE_N] [STEP_SIZE]
#
# 选项:
#   --diff     输出对比表格
#   --stress   高压力预设 (N=2000000 ROUNDS=10 STRIDE_N=500000 STEP_SIZE=1024 CONFIG=20000)
#   --light    低压力预设 (N=50000 ROUNDS=3 STRIDE_N=10000 STEP_SIZE=128 CONFIG=1000)
#   -h, --help 显示帮助
#
# 位置参数 (透传给 gc_bench.lua):
#   N            主测试对象数量         默认 200000
#   ROUNDS       每项测试重复轮数       默认 5
#   STRIDE_N     字符串/闭包测试数量    默认 50000
#   STEP_SIZE    增量GC单步的stepsize   默认 256
#   CONFIG_COUNT 配置工作集对象数量     默认 5000
#
# 示例:
#   bash tools/gc_bench_run.sh --diff                  # 默认压力 + 对比
#   bash tools/gc_bench_run.sh --stress --diff         # 高压力 + 对比
#   bash tools/gc_bench_run.sh -- 1000000 10           # 自定义参数

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/tests_lua/gc_bench.lua"
C_LUA="$ROOT/build/lua"
RS_LUA="$ROOT/target/release/lua"
OUT_DIR="${TMPDIR:-/tmp}"
C_OUT="$OUT_DIR/gc_bench_c.txt"
RS_OUT="$OUT_DIR/gc_bench_rs.txt"

# 确保可执行文件存在
if [ ! -x "$C_LUA" ]; then
    echo "错误: C 实现未构建 ($C_LUA)"
    echo "请先在 build/ 目录构建: cmake --build build"
    exit 1
fi
if [ ! -x "$RS_LUA" ]; then
    echo "错误: Rust 实现未构建 ($RS_LUA)"
    echo "请先构建: cargo build --release"
    exit 1
fi

# 解析选项
DO_DIFF=0
LUA_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --diff)   DO_DIFF=1; shift ;;
        --stress) LUA_ARGS=(2000000 10 500000 1024 20000); shift ;;
        --light)  LUA_ARGS=(50000 3 10000 128 1000); shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print} !/^#/{exit}' "$0"
            exit 0 ;;
        --)       shift; while [ $# -gt 0 ]; do LUA_ARGS+=("$1"); shift; done ;;
        *)        LUA_ARGS+=("$1"); shift ;;
    esac
done

echo ">>> 运行 C 实现测试 ($C_LUA) ${LUA_ARGS[*]} ..."
"$C_LUA" "$SCRIPT" "${LUA_ARGS[@]}" 2>&1 | tee "$C_OUT"

echo ""
echo ">>> 运行 Rust 实现测试 ($RS_LUA) ${LUA_ARGS[*]} ..."
"$RS_LUA" "$SCRIPT" "${LUA_ARGS[@]}" 2>&1 | tee "$RS_OUT"

echo ""
echo ">>> 测试完成。结果已保存:"
echo "  C 实现:    $C_OUT"
echo "  Rust 实现: $RS_OUT"

# 可选: 对比关键指标
if [ "$DO_DIFF" = "1" ]; then
    echo ""
    echo "================ 关键指标对比 ================"
    printf "%-28s %20s %20s\n" "指标" "C" "Rust"
    echo "-------------------------------------------------------------------------"

    # 提取 ">> 指标: 值" 行进行对比
    paste <(grep '>>' "$C_OUT") <(grep '>>' "$RS_OUT") | \
    while IFS=$'\t' read -r c_line r_line; do
        metric=$(echo "$c_line" | sed 's/^  >> *//; s/:.*//')
        c_val=$(echo "$c_line" | sed 's/^.*: *//')
        r_val=$(echo "$r_line" | sed 's/^.*: *//')
        printf "%-28s %20s %20s\n" "$metric" "$c_val" "$r_val"
    done
    echo "========================================================================="
fi
