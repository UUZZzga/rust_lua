#!/bin/bash
# 数值类型配置验证 (对应 C luaconf.h 的 LUA_INT_TYPE / LUA_FLOAT_TYPE)
#
# 遍历 4 种组合, 每种都: 构建 release → 跑 src_rs 单元/集成测试编译检查 → 跑冒烟脚本。
#   默认        : LuaInteger = i64, LuaNumber = f64   (C 的默认: long long + double)
#   num-i32     : LuaInteger = i32                    (C: LUA_INT_TYPE = LUA_INT_INT)
#   num-f32     : LuaNumber  = f32                    (C: LUA_FLOAT_TYPE = LUA_FLOAT_FLOAT)
#   embedded    : i32 + f32                           (C: LUA_32BITS)
#
# 用法:
#   bash tools/verify_num_types.sh            # 构建 + 冒烟 (较快)
#   bash tools/verify_num_types.sh --full     # 额外跑 cargo test (慢)
#
# 冒烟脚本: tests_lua/num_types_smoke.lua (断言与位宽无关, 4 种组合都必须通过)
set -u

cd "$(dirname "$0")/.." || exit 2
mkdir -p logs

FULL=0
[ "${1:-}" = "--full" ] && FULL=1

# 构建命令包装: 用 systemd-run 绕过内存限制 (CLAUDE.md 规则);
# 无 user systemd 会话的环境 (CI) 自动降级为直接运行。
run_build() {
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
        systemd-run --user --wait --collect --pipe \
            --property=LimitAS=infinity \
            --working-directory="$(pwd)" \
            "$@"
    else
        "$@"
    fi
}

# 运行 Lua 脚本: 200MB 内存限制 + 超时 (CLAUDE.md 规则)。
run_lua() {
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
        timeout 60 systemd-run --user --wait --collect --pipe \
            --property=LimitAS=204800 \
            --working-directory="$(pwd)" \
            ./target/release/lua "$@"
    else
        timeout 60 ./target/release/lua "$@"
    fi
}

CONFIGS=("" "num-i32" "num-f32" "embedded")
FAIL=0
SUMMARY=""

for cfg in "${CONFIGS[@]}"; do
    label="${cfg:-default(i64+f64)}"
    echo "============================================================"
    echo "[$label] building..."
    if [ -z "$cfg" ]; then
        run_build cargo build --release > "logs/numtypes_build_${label}.log" 2>&1
    else
        run_build cargo build --release --features "$cfg" > "logs/numtypes_build_${label}.log" 2>&1
    fi
    if [ $? -ne 0 ]; then
        echo "[$label] BUILD FAILED (see logs/numtypes_build_${label}.log)"
        FAIL=1
        SUMMARY="$SUMMARY\n  $label: BUILD FAILED"
        continue
    fi

    echo "[$label] smoke test..."
    if run_lua tests_lua/num_types_smoke.lua > "logs/numtypes_smoke_${label}.log" 2>&1; then
        result="$(tail -n 1 "logs/numtypes_smoke_${label}.log")"
        echo "[$label] smoke OK: $result"
    else
        echo "[$label] SMOKE FAILED (see logs/numtypes_smoke_${label}.log)"
        tail -n 20 "logs/numtypes_smoke_${label}.log"
        FAIL=1
        SUMMARY="$SUMMARY\n  $label: SMOKE FAILED"
        continue
    fi

    # 测试代码在各配置下必须能编译 (宽度相关断言由 cfg 门控)
    echo "[$label] checking test targets..."
    if [ -z "$cfg" ]; then
        run_build cargo check --release --lib --tests --features cmp_c > "logs/numtypes_check_${label}.log" 2>&1
    else
        run_build cargo check --release --lib --tests --features "$cfg" > "logs/numtypes_check_${label}.log" 2>&1
    fi
    if [ $? -ne 0 ]; then
        echo "[$label] TEST TARGETS DO NOT COMPILE (see logs/numtypes_check_${label}.log)"
        FAIL=1
        SUMMARY="$SUMMARY\n  $label: test targets do not compile"
        continue
    fi

    if [ "$FULL" = "1" ]; then
        echo "[$label] cargo test..."
        if [ -z "$cfg" ]; then
            run_build cargo test --release > "logs/numtypes_test_${label}.log" 2>&1
        else
            run_build cargo test --release --features "$cfg" > "logs/numtypes_test_${label}.log" 2>&1
        fi
        if [ $? -ne 0 ] || grep -q "test result: FAILED" "logs/numtypes_test_${label}.log"; then
            echo "[$label] TESTS FAILED (see logs/numtypes_test_${label}.log)"
            FAIL=1
            SUMMARY="$SUMMARY\n  $label: tests failed"
            continue
        fi
    fi

    SUMMARY="$SUMMARY\n  $label: OK"
done

echo "============================================================"
echo -e "SUMMARY:$SUMMARY"

# 收尾: 把 target/release/lua 还原为默认配置 (64 位 i64/f64), 避免留下嵌入式二进制
echo "rebuilding default configuration (i64 + f64)..."
run_build cargo build --release > "logs/numtypes_build_restore.log" 2>&1 || echo "WARNING: default rebuild failed"

[ "$FAIL" = "1" ] && exit 2
echo "all number-type configurations passed"
