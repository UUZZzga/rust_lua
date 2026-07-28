#!/bin/bash
# Miri 未定义行为 (UB) 检测脚本
#
# 用途: 在 nightly 工具链上用 Miri 解释执行 Rust 测试,检测:
#   - 越界内存访问 / use-after-free
#   - 使用未初始化内存
#   - 栈借用 (Tree Borrows) 违规
#   - 数据竞争 (本项目主要单线程,检测面有限)
#   - 整数溢出等 UB
#
# 覆盖范围:
#   - tests_rs/ 下 7 个集成测试文件
#   - src_rs/ 中 22 个含 #[cfg(test)] 的模块单元测试
#   - 跳过 ffi feature (链接 C liblua,Miri 无法解释)
#   - 跳过 compiler/cmp_tests (依赖 ffi feature)
#   - 跳过 tests_rs/integration_tests::test_stdin_input (Command::spawn)
#
# 使用方式:
#   bash tools/miri.sh                # 运行全部可检测测试
#   bash tools/miri.sh --setup        # 仅安装 nightly + miri 组件 (首次使用)
#   bash tools/miri.sh --filter math  # 仅运行名称含 "math" 的测试
#   bash tools/miri.sh --lib          # 仅运行库单元测试 (src_rs)
#   bash tools/miri.sh --no-log       # 不写日志,直接输出到终端
#
# 输出: logs/miri_test.log
#
# 注意:
#   - Miri 比正常测试慢 10-100 倍,完整运行可能需要 10+ 分钟
#   - 默认使用 Tree Borrows 模型 (比 Stacked Borrows 宽松,适合 Rc 重度使用)
#   - 内存限制: Miri 解释执行内存占用大,用 LimitAS=infinity 绕过 (属构建+测试混合)
#
# 已知限制:
#   - io 库在 Miri 下跳过初始化 (extern static stdin 不支持)
#   - tests_rs/integration_tests::test_stdin_execution 被跳过 (Command::spawn)
#   - 信号处理在 Miri 下跳过 (sigemptyset/sigaction 不支持)
#   - 内存泄漏被忽略 (-Zmiri-ignore-leaks,Rc 循环引用是已知设计特性)
#   详见 CLAUDE.md "Miri 检测规则" 章节

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

# ============================================================================
# 参数解析
# ============================================================================
SETUP_ONLY=0
FILTER=""
LIB_ONLY=0
WRITE_LOG=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --setup)
            SETUP_ONLY=1
            shift
            ;;
        --filter)
            FILTER="$2"
            shift 2
            ;;
        --lib)
            LIB_ONLY=1
            shift
            ;;
        --no-log)
            WRITE_LOG=0
            shift
            ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "ERROR: 未知参数 $1"
            echo "使用 --help 查看用法"
            exit 2
            ;;
    esac
done

# ============================================================================
# 1. 检查 nightly 工具链与 miri 组件
# ============================================================================
echo "Checking nightly toolchain and miri component..."

if ! command -v rustup >/dev/null 2>&1; then
    echo "ERROR: 未找到 rustup,请先安装 rustup"
    exit 2
fi

# 检查 nightly 工具链是否已安装
NIGHTLY_INSTALLED=$(rustup toolchain list 2>/dev/null | grep -c 'nightly' || true)
if [ "$NIGHTLY_INSTALLED" -eq 0 ]; then
    echo "Nightly toolchain not installed, installing..."
    rustup toolchain install nightly --profile minimal --component miri
else
    # 检查 miri 组件
    MIRI_INSTALLED=$(rustup component list --toolchain nightly 2>/dev/null | grep -c '^miri.*installed' || true)
    if [ "$MIRI_INSTALLED" -eq 0 ]; then
        echo "Miri component not installed, adding to nightly..."
        rustup component add miri --toolchain nightly
    fi
fi

echo "Toolchain ready: $(rustc +nightly --version)"
echo "Miri: $(cargo +nightly miri --version 2>/dev/null || echo 'setup required')"

if [ "$SETUP_ONLY" -eq 1 ]; then
    echo "Setup complete. Run 'bash tools/miri.sh' to start detection."
    exit 0
fi

# ============================================================================
# 2. 配置 MIRIFLAGS
# ============================================================================
# -Zmiri-tree-borrows: 使用 Tree Borrows 模型 (比 Stacked Borrows 宽松,
#                      适合本项目 Rc/Arc 重度共享场景,减少误报)
# -Zmiri-disable-isolation: 禁用环境隔离,允许访问文件系统/环境变量
#                           (测试代码用 std::env!() / env!("CARGO_MANIFEST_DIR"))
# -Zmiri-ignore-leaks: 忽略内存泄漏报告 (非 UB)。本项目 LuaState 中 Rc 循环
#                      引用 (如 metatable/registry) 会导致测试结束时内存未释放,
#                      这是已知设计特性,不是 bug。Miri 专注于检测真正的 UB。
export MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-disable-isolation -Zmiri-ignore-leaks"

# ============================================================================
# 3. 构建 cargo miri test 命令
# ============================================================================
# 不启用 ffi feature: ffi 链接 C liblua,Miri 无法解释 C 代码
# 不运行 compiler/cmp_tests: 依赖 ffi feature
MIRI_CMD=(cargo +nightly miri test)

# 测试过滤
if [ -n "$FILTER" ]; then
    MIRI_CMD+=("$FILTER")
fi

# 仅运行库单元测试 (src_rs/ 内的 #[cfg(test)] 模块)
if [ "$LIB_ONLY" -eq 1 ]; then
    MIRI_CMD+=(--lib)
fi

# ============================================================================
# 4. 运行 (用 systemd-run --property=LimitAS=infinity 包装,属构建+测试混合)
#    - CI 等无 user systemd 会话环境自动降级为直接运行
#    - timeout 1800: Miri 解释执行慢,给 30 分钟上限
# ============================================================================
run_miri() {
    if command -v systemd-run >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
        systemd-run --user --wait --collect --pipe \
            --property=LimitAS=infinity \
            --working-directory="$PROJECT_ROOT" \
            env MIRIFLAGS="$MIRIFLAGS" \
            timeout 1800 "${MIRI_CMD[@]}"
    else
        MIRIFLAGS="$MIRIFLAGS" timeout 1800 "${MIRI_CMD[@]}"
    fi
}

mkdir -p logs

if [ "$WRITE_LOG" -eq 1 ]; then
    echo "Running Miri tests (output -> logs/miri_test.log)..."
    echo "Command: ${MIRI_CMD[*]}"
    echo "MIRIFLAGS: $MIRIFLAGS"
    echo ""
    if ! run_miri > logs/miri_test.log 2>&1 < /dev/null; then
        echo "ERROR: Miri tests failed!"
        echo "Check logs/miri_test.log for details"
        # 显示失败的关键行 (error/panic/UB 报告)
        echo ""
        echo "=== 关键错误摘要 (前 50 行) ==="
        grep -nE '^(error|ERROR|thread .* panicked|Undefined Behavior|warning: unsupported)' \
            logs/miri_test.log 2>/dev/null | head -50 || true
        exit 2
    fi

    # 检查是否有失败的测试
    if grep -q "test result: FAILED" logs/miri_test.log 2>/dev/null; then
        echo "ERROR: Some tests failed in Miri!"
        grep -E "^test result:|FAILURES:" logs/miri_test.log | head -20
        echo "Check logs/miri_test.log for details"
        exit 2
    fi
else
    echo "Running Miri tests (direct output)..."
    run_miri < /dev/null
fi

echo ""
echo "Miri tests passed!"
echo "  - 检测模型: Tree Borrows"
echo "  - 检测范围: tests_rs + src_rs 单元测试 (ffi feature 关闭)"
echo "  - 完整日志: logs/miri_test.log"
exit 0
