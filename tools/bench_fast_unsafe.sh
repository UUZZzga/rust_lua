#!/bin/bash
# bench_fast_unsafe.sh — 量化 fast_unsafe feature (VM 热路径 unsafe 优化) 的性能差距
#
# 用法:
#   bash tools/bench_fast_unsafe.sh [quick|full] [轮数]
#
#   位置参数:
#     quick|full  基准规模 (默认 quick)
#     轮数        交错采样轮数 (默认: linux full=5, 其他=3)
#   选项:
#     -h, --help  显示帮助
#
# 做法:
#   1. 构建 fast_unsafe=on 的 release 二进制 (cargo build --release)
#   2. 构建 fast_unsafe=off 的 release 二进制 (cargo build --release
#      --no-default-features), 两者分别拷出到 logs/fast_unsafe_ab/
#   3. 恢复默认 (on) 构建, 使 target/release/lua 与仓库默认配置一致
#   4. 交错采样: 每轮"先 on 后 off"紧挨着跑同一套 bench (BENCH_REPEAT=1),
#      最后对每个指标取全部轮次的最小值后对比 (与 ci_bench.sh 同口径 —
#      同轮内两侧经历几乎相同的热状态/调度噪声, 比值噪声远小于块式采样)
#
# 输出: 每指标 on/off 最小耗时 + off/on 比值 + 几何平均 (off/on > 1 表示
#       关闭 fast_unsafe 后更慢, 即该优化的实际收益)。

set -o pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

case "$(uname -s)" in
    *MINGW*|*CYGWIN*|*MSYS*) EXE=.exe; PLATFORM=windows ;;
    Linux*)                  EXE=;    PLATFORM=linux ;;
    *) echo "bench_fast_unsafe.sh: 未知平台 $(uname -s)"; exit 1 ;;
esac

SCALE=quick
ROUNDS=""
while [ $# -gt 0 ]; do
    case "$1" in
        quick|full) SCALE="$1"; shift ;;
        [0-9]*)     ROUNDS="$1"; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *)
            echo "bench_fast_unsafe.sh: 未知参数 '$1' (用法: bash tools/bench_fast_unsafe.sh [quick|full] [轮数])"
            exit 2 ;;
    esac
done
if [ -z "$ROUNDS" ]; then
    if [ "$SCALE" = "full" ] && [ "$PLATFORM" = "linux" ]; then
        ROUNDS=5
    else
        ROUNDS=3
    fi
fi

ls bench/bench_*.lua >/dev/null 2>&1 || { echo "错误: bench/bench_*.lua 不存在"; exit 1; }

mkdir -p logs
AB_DIR="logs/fast_unsafe_ab"
mkdir -p "$AB_DIR"
ON_BIN="$AB_DIR/lua_fast_unsafe$EXE"
OFF_BIN="$AB_DIR/lua_no_fast_unsafe$EXE"

# 复用仓库 target/ 目录 (cargo fingerprint 按 feature 区分, 切换变体只重编 lua-rs
# 本身, 依赖不重复编译); 结束时恢复默认构建, 使 target/release/lua 回到 on 变体。
# 两侧都用普通 release 构建 (不带 PGO/abort): 比值只反映 fast_unsafe 本身.
echo ">>> 构建 fast_unsafe=on ..."
cargo build --release || exit 1
cp "target/release/lua$EXE" "$ON_BIN" || exit 1

echo ">>> 构建 fast_unsafe=off (--no-default-features) ..."
cargo build --release --no-default-features || exit 1
cp "target/release/lua$EXE" "$OFF_BIN" || exit 1

echo ">>> 恢复默认 (fast_unsafe=on) 构建 ..."
cargo build --release || exit 1

ON_OUT="$AB_DIR/on_${PLATFORM}.txt"
OFF_OUT="$AB_DIR/off_${PLATFORM}.txt"
: > "$ON_OUT"
: > "$OFF_OUT"
uname -a > "$ON_OUT"; "$ON_BIN" -v >> "$ON_OUT" 2>&1
uname -a > "$OFF_OUT"; "$OFF_BIN" -v >> "$OFF_OUT" 2>&1

echo ">>> 交错采样 fast_unsafe on/off ($PLATFORM, $SCALE, $ROUNDS 轮) ..."
export BENCH_REPEAT=1
for r in $(seq 1 "$ROUNDS"); do
    echo "--- round $r/$ROUNDS: fast_unsafe=on ---"
    timeout 900 "$ON_BIN" bench/harness.lua "$SCALE" bench/bench_*.lua 2>&1 | tee -a "$ON_OUT"
    rc=$?
    if [ "$rc" -ne 0 ]; then echo "错误: on 侧基准失败 (退出码 $rc, 轮 $r, 日志 $ON_OUT)"; exit "$rc"; fi
    echo "--- round $r/$ROUNDS: fast_unsafe=off ---"
    timeout 900 "$OFF_BIN" bench/harness.lua "$SCALE" bench/bench_*.lua 2>&1 | tee -a "$OFF_OUT"
    rc=$?
    if [ "$rc" -ne 0 ]; then echo "错误: off 侧基准失败 (退出码 $rc, 轮 $r, 日志 $OFF_OUT)"; exit "$rc"; fi
done

echo ""
echo "=========== fast_unsafe 性能差距 ($PLATFORM, $SCALE, ${ROUNDS} 轮交错取min) ==========="
printf "%-28s %14s %14s %12s\n" "指标" "on (默认)" "off (关闭)" "off/on"
echo "---------------------------------------------------------------------------------"
awk -v onf="$ON_OUT" -v offf="$OFF_OUT" '
    /^  >> / {
        line = $0
        sub(/\r$/, "", line)
        sub(/^  >> /, "", line)
        pos = match(line, /: [0-9.]+$/)
        if (pos == 0) next
        key = substr(line, 1, pos - 1)
        val = substr(line, pos + 2) + 0
        which = (FILENAME == onf) ? "on" : "off"
        sk = which SUBSEP key
        if (!(sk in seen) || val < memo[sk]) memo[sk] = val
        seen[sk] = 1
        if (!(key in oseen)) { order[++n] = key; oseen[key] = 1 }
    }
    END {
        logsum = 0; m = 0
        for (i = 1; i <= n; i++) {
            k = order[i]
            a = memo["on" SUBSEP k]; b = memo["off" SUBSEP k]
            if (a > 0 && b > 0) {
                printf "%-28s %14.4f %14.4f %11.3fx\n", k, a, b, b / a
                logsum += log(b / a); m++
            } else {
                printf "%-28s %14.4f %14.4f %12s\n", k, a, b, "-"
            }
        }
        if (m > 0) {
            g = exp(logsum / m)
            printf "\n几何平均 off/on = %.3fx (关闭 fast_unsafe 后耗时倍数, 即平均慢 %.1f%%)\n", g, (g - 1) * 100
        }
    }
' "$ON_OUT" "$OFF_OUT"
echo "================================================================================="
echo "  原始日志: $ON_OUT / $OFF_OUT"
