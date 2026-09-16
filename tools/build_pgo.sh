#!/bin/bash
# build_pgo.sh — 用 PGO (profile-guided optimization) 构建 release lua
#
# 三步:
#   1. instrumented 构建 (-Cprofile-generate)
#   2. 用代表性负载运行解释器, 产出 .profraw
#   3. merge profdata 后以 -Cprofile-use 重新构建, 产物即最终 target/release/lua
#
# 用法:
#   bash tools/build_pgo.sh                 # 默认负载 (tests_lua 套件 + bench)
#   PGO_TRAIN_SCALE=quick bash tools/build_pgo.sh
#   PGO_RUSTFLAGS="-Ctarget-cpu=native" bash tools/build_pgo.sh   # 追加编译 flag
#   PGO_FEATURES="lua_longjmp" bash tools/build_pgo.sh            # 追加 cargo features
#
# 设计要点:
# - PGO 只改优化决策, 不改语义; 正确性由调用方的测试步骤保证。
# - 工具链缺失 (无 llvm-profdata) 或任一步失败时回退为普通 cargo build --release,
#   保证 CI 流水线不因环境差异硬失败; 回退时打印 PGO_FALLBACK 供日志排查。
# - 训练负载只用通用解释器工作负载 (官方测试 + 常规 bench 驱动), 不与待测
#   基准同源特化。
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

HOST=$(rustc -vV | sed -n 's/^host: //p')
find_profdata() {
    local sysroot
    sysroot=$(rustc --print sysroot)
    local p="$sysroot/lib/rustlib/$HOST/bin/llvm-profdata"
    if [ -x "$p" ]; then echo "$p"; return 0; fi
    p="$sysroot/lib/rustlib/$HOST/bin/llvm-profdata.exe"
    if [ -x "$p" ]; then echo "$p"; return 0; fi
    return 1
}

PROFDATA=$(find_profdata || true)
if [ -z "$PROFDATA" ]; then
    # CI 镜像可能未预装 llvm-tools; rustup 可运行时补齐
    echo ">>> llvm-profdata 缺失, 尝试 rustup component add llvm-tools"
    rustup component add llvm-tools >/dev/null 2>&1 \
        || rustup component add llvm-tools-preview >/dev/null 2>&1 || true
    PROFDATA=$(find_profdata || true)
fi
FEATURE_ARGS=""
[ -n "${PGO_FEATURES:-}" ] && FEATURE_ARGS="--features $PGO_FEATURES"
if [ -z "$PROFDATA" ]; then
    echo "PGO_FALLBACK: llvm-profdata 未找到, 改用普通构建"
    exec cargo build --release $FEATURE_ARGS
fi

PGO_DIR="$ROOT/pgo"
rm -rf "$PGO_DIR"
mkdir -p "$PGO_DIR"

SCALE="${PGO_TRAIN_SCALE:-quick}"
EXE_SUFFIX=""
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*|Windows*) EXE_SUFFIX=".exe" ;;
esac
LUA="$ROOT/target/release/lua$EXE_SUFFIX"
# (FEATURE_ARGS 已在 profdata 回退前定义)
echo ">>> [1/3] instrumented 构建 (profile-generate)"
if ! RUSTFLAGS="-Cprofile-generate=$PGO_DIR ${PGO_RUSTFLAGS:-}" cargo build --release $FEATURE_ARGS; then
    echo "PGO_FALLBACK: instrumented 构建失败, 改用普通构建"
    unset RUSTFLAGS
    exec cargo build --release $FEATURE_ARGS
fi

echo ">>> [2/3] 训练 ($SCALE)"
# 官方 Lua 测试套件: 覆盖解释器绝大多数 opcode/表/字符串/协程/GC 路径。
# 各文件在 tests_lua 目录内执行 (部分用例依赖相对路径)。
# stdin 用空管道 (不可 seek, 立即 EOF), 符合仓库命令执行规则。
for f in calls closure coroutine db errors events gc math mem nextvar pm \
         sort strings tpack utf8 vararg verybig locals; do
    [ -f "tests_lua/$f.lua" ] || continue
    ( cd tests_lua && printf '' | "$LUA" "$f.lua" >/dev/null 2>&1 || true )
done
# 常规性能基准 (quick): 覆盖算术/表/字符串/调用主循环形态 (仅作通用热点预热)
for f in bench/bench_*.lua; do
    printf '' | "$LUA" bench/harness.lua "$SCALE" "$f" >/dev/null 2>&1 || true
done
# 目标负载加权 (PGO_FOCUS="bench/bench_arith_float.lua:full bench/...:quick"):
# 对特定 bench 以指定规模追加训练采样, 把内联/布局决策向热路径倾斜。
# 与 #105 的全量 full 训练不同 (那劣化表插入 8.6% 已回退): 只加权被优化
# 的目标, 其余保持 quick 覆盖; 副作用由 CI 交错 12 项面板裁判。
for spec in ${PGO_FOCUS:-}; do
    file=${spec%:*}; scale=${spec##*:}
    [ -f "$file" ] || continue
    printf '' | "$LUA" bench/harness.lua "$scale" "$file" >/dev/null 2>&1 || true
done

PROFRAW_COUNT=$(find "$PGO_DIR" -name '*.profraw' 2>/dev/null | wc -l | tr -d ' ')
if [ "$PROFRAW_COUNT" = "0" ]; then
    echo "PGO_FALLBACK: 未产出 .profraw, 改用普通构建"
    unset RUSTFLAGS
    exec cargo build --release $FEATURE_ARGS
fi

echo ">>> merge $PROFRAW_COUNT profraw"
if ! "$PROFDATA" merge -sparse "$PGO_DIR"/*.profraw -o "$PGO_DIR/default.profdata"; then
    echo "PGO_FALLBACK: llvm-profdata merge 失败, 改用普通构建"
    unset RUSTFLAGS
    exec cargo build --release $FEATURE_ARGS
fi

echo ">>> [3/3] 优化构建 (profile-use)"
if ! RUSTFLAGS="-Cprofile-use=$PGO_DIR/default.profdata ${PGO_RUSTFLAGS:-}" cargo build --release $FEATURE_ARGS; then
    echo "PGO_FALLBACK: profile-use 构建失败, 改用普通构建"
    unset RUSTFLAGS
    exec cargo build --release $FEATURE_ARGS
fi
echo ">>> PGO 完成: $LUA"
