#!/bin/bash
# 内存分配失败测试（**主动**限制进程地址空间为 200MB）—— 对应 tests_lua/alloc_fail.lua
#
# 与 C 的 tests_lua/memerr.lua 等价（C 版靠测试库 T/ltests 精确制造失败）：
# Rust 实现不编译 ltests，改为在受限地址空间下真实耗尽内存，验证
#   1) 内存耗尽不崩溃（不 abort、不被信号杀死）；
#   2) 先紧急全量 GC 再重试分配（lmem.c 的 tryagain / luaC_fullgc(L,1)）；
#   3) 重试仍失败时抛可捕获的内存错误 "not enough memory"（ldo.c 的 LUA_ERRMEM）；
#   4) 错误之后解释器仍然可用。
#
# 说明：本脚本**独立运行**（尚未接入 tools/verify.sh / CI）——当前解释器仍有若干
# 分配点未纳入漏斗（帧入口栈扩容 `fast_resize_stack_nil` / `pad_frame_scratch_slow`、
# capi 的 settop/checkstack、`.to_string()` 构造的错误/回溯字符串等），真实耗尽内存时
# 这些点仍会 abort，本脚本会如实报失败。确定性的分配失败覆盖（不依赖真实耗尽）见
# tests_rs/alloc_failure_tests.rs（已由 `cargo test` 纳入 CI）。
#
# 本地/CI 统一入口（**所有**执行都带 200MB 上限，不允许无限运行 ——
# 无限运行时本测试会真的吃满宿主内存）：
#   bash tools/verify_alloc_fail.sh
# 环境变量：LUA_EXEC（默认 ./target/release/lua）、ALLOC_FAIL_TIMEOUT（默认 60 秒）
set -u

LUA_EXEC=${LUA_EXEC:-./target/release/lua}
LIMIT_KB=204800                       # 200MB（CLAUDE.md 的内存限制规则）
TIMEOUT=${ALLOC_FAIL_TIMEOUT:-60}
LOG_DIR=logs
mkdir -p "$LOG_DIR"

if [ ! -x "$LUA_EXEC" ]; then
    echo "ERROR: 解释器不存在: $LUA_EXEC（先运行 cargo build --release）" >&2
    exit 1
fi

# 带 200MB 地址空间上限执行命令；无法施加限制时返回 127（调用方跳过）。
#
# - 先试 `ulimit -v`（shell 内置；容器/CI 通常可用），在**子 shell** 内设置并回读确认；
# - 否则退回 `systemd-run --property=LimitAS=204800`（与仓库其余测试一致）；
# - 两者都不可用（如 Windows/MSYS）：返回 127，绝不无限运行。
run_limited() {
    if ( ulimit -v "$LIMIT_KB" ) 2>/dev/null; then
        ( ulimit -v "$LIMIT_KB"; exec "$@" )
        return $?
    fi
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
        systemd-run --user --wait --collect --pipe \
            --property=LimitAS=$LIMIT_KB \
            --working-directory="$(pwd)" \
            "$@"
        return $?
    fi
    return 127
}

echo "=== alloc_fail.lua (LimitAS=${LIMIT_KB}KB) ==="
# 先删旧日志: 跳过分支（无法设置上限）不得被上一次运行留下的崩溃特征误判
rm -f "$LOG_DIR/alloc_fail_run.log"
run_limited timeout "$TIMEOUT" "$LUA_EXEC" tests_lua/alloc_fail.lua \
    > "$LOG_DIR/alloc_fail_run.log" 2>&1 < /dev/null
STATUS=$?

# 崩溃检测最先做：退出码 >=128 是信号死亡；MSYS/Git-Bash 下 abort 会被折叠成 127
# （与「无法设置上限」同码），因此必须按日志特征判定优先区分。
# Rust 的分配失败 abort（handle_alloc_error）与栈溢出都会打印这两条固定文本。
if [ "$STATUS" -ge 128 ] || grep -qE "memory allocation of [0-9]+ bytes failed|has overflowed its stack" "$LOG_DIR/alloc_fail_run.log"; then
    echo "ERROR: alloc_fail.lua 崩溃（status=$STATUS）—— 内存耗尽必须报错而不是 abort/溢出！"
    tail -20 "$LOG_DIR/alloc_fail_run.log"
    exit 2
fi
if [ "$STATUS" -eq 127 ]; then
    echo "跳过 alloc_fail.lua：当前环境无法设置地址空间上限（需要 ulimit -v 或 user systemd）"
    echo "注意：不设上限运行该测试会真的耗尽宿主内存，故不执行。"
    exit 0
fi
if [ "$STATUS" -ne 0 ]; then
    echo "ERROR: alloc_fail.lua 失败 (exit code $STATUS)，详见 $LOG_DIR/alloc_fail_run.log"
    tail -20 "$LOG_DIR/alloc_fail_run.log"
    exit 2
fi
if ! grep -q "^Ok$" "$LOG_DIR/alloc_fail_run.log"; then
    echo "ERROR: alloc_fail.lua 输出缺少 Ok 行（详见 $LOG_DIR/alloc_fail_run.log）"
    tail -20 "$LOG_DIR/alloc_fail_run.log"
    exit 2
fi

echo "=== 未捕获的内存错误（顶层 pcall 之外）==="
# 对应 C 的 lua.c：内存错误未被 pcall 捕获时，解释器必须打印
# "not enough memory" 并以非零退出码结束（而不是 abort/segfault）。
TOPLEVEL_SCRIPT=$(mktemp) || exit 1
rm -f "$LOG_DIR/alloc_fail_toplevel.log"
cat > "$TOPLEVEL_SCRIPT" <<'LUA'
-- 不可回收的增长：一直可达 → 紧急 GC 无法挽救 → 内存错误直达顶层
local t = {}
for i = 1, math.maxinteger do t[i] = {} end
LUA
run_limited timeout "$TIMEOUT" "$LUA_EXEC" "$TOPLEVEL_SCRIPT" \
    > "$LOG_DIR/alloc_fail_toplevel.log" 2>&1 < /dev/null
STATUS=$?
rm -f "$TOPLEVEL_SCRIPT"

if [ "$STATUS" -ge 128 ] || grep -qE "memory allocation of [0-9]+ bytes failed|has overflowed its stack" "$LOG_DIR/alloc_fail_toplevel.log"; then
    echo "ERROR: 顶层内存错误导致崩溃（status=$STATUS）—— 必须打印消息并非零退出"
    tail -10 "$LOG_DIR/alloc_fail_toplevel.log"
    exit 2
fi
if [ "$STATUS" -eq 127 ]; then
    echo "跳过顶层内存错误测试：无法设置地址空间上限"
    exit 0
fi
if [ "$STATUS" -eq 0 ]; then
    echo "ERROR: 顶层内存耗尽却以 0 退出（预期非零退出码）"
    tail -10 "$LOG_DIR/alloc_fail_toplevel.log"
    exit 2
fi
if ! grep -q "not enough memory" "$LOG_DIR/alloc_fail_toplevel.log"; then
    echo "ERROR: 顶层内存错误消息不匹配（预期含 'not enough memory'）"
    tail -10 "$LOG_DIR/alloc_fail_toplevel.log"
    exit 2
fi

echo "Alloc-failure tests passed."
exit 0