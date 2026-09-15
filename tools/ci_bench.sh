#!/bin/bash
# ci_bench.sh — 运行 bench/ 下按项拆分的性能基准, 对比 C 与 Rust 实现
#
# 用法:
#   bash tools/ci_bench.sh [--skip-build] [quick|full]
#
# 选项:
#   --skip-build  只跳过 cargo build --release (C lua 的 cmake 构建始终执行, 幂等增量)
#   -h, --help    显示帮助
#
# 位置参数:
#   quick|full    基准规模 (默认 quick)
#
# 示例:
#   bash tools/ci_bench.sh quick             # 快速规模 + 先构建
#   bash tools/ci_bench.sh --skip-build full  # full 规模, 跳过 cargo 构建

set -o pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

# 平台与后缀探测
case "$(uname -s)" in
    *MINGW*|*CYGWIN*|*MSYS*) EXE=.exe; PLATFORM=windows ;;
    Linux*)                  EXE=;    PLATFORM=linux ;;
esac

# 解析选项
SCALE=quick
SKIP_BUILD=0
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1; shift ;;
        quick|full)   SCALE="$1"; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *) echo "ci_bench.sh: 未知参数 '$1' (用法: bash tools/ci_bench.sh [--skip-build] [quick|full])"; exit 2 ;;
    esac
done

# 构建
cmake -B build -S . -DCMAKE_BUILD_TYPE=Release || exit 1
if [ "$PLATFORM" = "linux" ]; then
    cmake --build build --parallel "$(nproc)" || exit 1
else
    cmake --build build --config Release || exit 1
fi
if [ "$SKIP_BUILD" != "1" ]; then
    # 未跳过构建时走平台自适应优化构建 (PGO + Linux 默认 panic=abort/lua_longjmp;
    # 决策内聚脚本内, 任一层缺失自动回退普通构建)。Windows 管线经
    # ci_bench_win.ps1 → 本脚本由此获得 PGO (不带 abort)。
    bash tools/build_release_opt.sh || exit 1
fi

# C 实现路径探测 (build/lua -> build/Release/lua)
C_LUA=""
for cand in "build/lua$EXE" "build/Release/lua$EXE"; do
    if [ -x "$cand" ]; then C_LUA="$cand"; break; fi
done
if [ -z "$C_LUA" ]; then
    echo "错误: C 实现未构建 (build/lua$EXE 或 build/Release/lua$EXE)"
    echo "请先构建: cmake -B build -S . -DCMAKE_BUILD_TYPE=Release && cmake --build build"
    exit 1
fi
RS_LUA="target/release/lua$EXE"
if [ ! -x "$RS_LUA" ]; then
    echo "错误: Rust 实现未构建 ($RS_LUA)"
    echo "请先构建: cargo build --release"
    exit 1
fi

# 基准文件存在性
ls bench/bench_*.lua >/dev/null 2>&1 || { echo "错误: bench/bench_*.lua 不存在"; exit 1; }

mkdir -p logs
C_OUT="logs/ci_bench_${PLATFORM}_c.txt"
RS_OUT="logs/ci_bench_${PLATFORM}_rs.txt"

# 日志头
uname -a > "$C_OUT"
"$C_LUA" -v >> "$C_OUT" 2>&1
uname -a > "$RS_OUT"
"$RS_LUA" -v >> "$RS_OUT" 2>&1

echo ">>> 运行 C/Rust 基准 (交错, $SCALE) ..."
# 交错采样: 每轮先 C 后 RS 紧挨着跑 (BENCH_REPEAT=1), 最后对每个指标取全部
# 轮次的【最小值】。同轮内两侧经历几乎相同的热状态/调度噪声 — 漂移变共模,
# 比值噪声从 ±5% (块式: C 全跑完再跑 RS, 中间机器状态漂移) 压到 ±1-2%。
# (块式 3 轮取 min 的同 commit 实证带: #120 1.0318 / #122 1.0861 / #123 1.0500
#  / #124 1.0256 — ±4.9%, 一切 <5% 的候选不可判定。)
if [ "$PLATFORM" = "windows" ] || [ "$SCALE" = "quick" ]; then
    ROUNDS=3
else
    ROUNDS=5
fi
export BENCH_REPEAT=1
for r in $(seq 1 "$ROUNDS"); do
    echo "--- round $r/$ROUNDS: C ---"
    timeout 900 "$C_LUA" bench/harness.lua "$SCALE" bench/bench_*.lua 2>&1 | tee -a "$C_OUT"
    C_RC=$?
    if [ "$C_RC" -ne 0 ]; then
        echo "错误: C 实现基准失败 (退出码 $C_RC, 轮 $r, 日志 $C_OUT)"
        exit "$C_RC"
    fi
    echo "--- round $r/$ROUNDS: Rust ---"
    timeout 900 "$RS_LUA" bench/harness.lua "$SCALE" bench/bench_*.lua 2>&1 | tee -a "$RS_OUT"
    RS_RC=$?
    if [ "$RS_RC" -ne 0 ]; then
        echo "错误: Rust 实现基准失败 (退出码 $RS_RC, 轮 $r, 日志 $RS_OUT)"
        exit "$RS_RC"
    fi
done

# 对比表: 按指标名聚合两侧全部 ">>" 行取最小值 (不再 paste 行配对)。
# awk 用 seen[who SUBSEP key] 单数组兼容 mawk/gawk (镜像默认 mawk, 不支持 a[x][y])。
echo ""
echo "================ 性能基准对比 ($PLATFORM, $SCALE, ${ROUNDS} 轮交错取min) ================"
printf "%-28s %20s %20s\n" "指标" "C" "Rust"
echo "-------------------------------------------------------------------------"
awk -v cfile="$C_OUT" '
    /^  >> / {
        line = $0
        sub(/^  >> /, "", line)
        pos = match(line, /: [0-9.]+$/)
        if (pos == 0) next
        key = substr(line, 1, pos - 1)
        val = substr(line, pos + 2) + 0
        which = (FILENAME == cfile) ? "c" : "r"
        sk = which SUBSEP key
        if (!(sk in seen) || val < memo[sk]) memo[sk] = val
        seen[sk] = 1
        if (!(key in oseen)) { order[++n] = key; oseen[key] = 1 }
    }
    END {
        for (i = 1; i <= n; i++) {
            k = order[i]
            printf "%-28s %20.4f %20.4f\n", k, memo["c" SUBSEP k], memo["r" SUBSEP k]
        }
    }
' "$C_OUT" "$RS_OUT"
echo "========================================================================="

# tests_lua/all.lua 计时对比 (仅 Linux; 其内部 dofile 的 main.lua 依赖 Unix shell, Windows 跳过)
# all.lua 自带计时打印 (每文件 "time: %g (+%g)", 结尾 "total time: %.2fs (wall time: %gs)"),
# 直接运行并提取其内部打印的 total time。
if [ "$PLATFORM" = "linux" ]; then
    echo ""
    echo ">>> tests_lua/all.lua 计时 (C vs Rust) ..."
    # main.lua (all.lua 内部 dofile) 的 lib2-v2 测试需要 tests_lua/libs/*.so (git 只跟踪 .c 源码)
    make -C tests_lua/libs >/dev/null 2>&1 || echo "警告: tests_lua/libs 编译失败, all.lua 可能不完整"
    # 运行 all.lua: 完整输出 tee 到日志文件, 只回显 grep 到的 total time (进命令替换)
    run_all_bench() {
        local lua="$1" tag="$2" abs_lua
        case "$lua" in
            /*) abs_lua="$lua" ;;
            *)  abs_lua="$PWD/$lua" ;;
        esac
        local logfile="logs/ci_bench_all_$tag.txt"
        ( cd tests_lua && timeout 600 "$abs_lua" all.lua 2>&1 ) | tee "$logfile" >&2
        local rc=${PIPESTATUS[0]}
        if [ "$rc" -ne 0 ]; then
            echo "all.lua ($tag) 退出码 $rc — 最后 5 行: " >&2
            tail -5 "$logfile" >&2
            return 1
        fi
        grep '^total time' "$logfile" | sed 's/total time: //'
    }
    C_MAIN=$(run_all_bench "$C_LUA" c)
    RS_MAIN=$(run_all_bench "$RS_LUA" rs)
    if [ -n "$C_MAIN" ] && [ -n "$RS_MAIN" ]; then
        printf "%-28s %20s %20s\n" "all.lua total time" "C" "Rust"
        printf "%-28s %20s %20s\n" "耗时 (秒)" "$C_MAIN" "$RS_MAIN"
    else
        echo "警告: all.lua 计时未完成 (C='$C_MAIN' Rust='$RS_MAIN'), 跳过对比"
    fi
fi
echo ""
echo ">>> 基准完成。结果已保存:"
echo "  C 实现:    $C_OUT"
echo "  Rust 实现: $RS_OUT"
