#!/bin/bash
# Hook 脚本：校验命令是否符合内存限制规则
# - 构建命令必须使用 systemd-run 绕过内存限制
# - 测试命令必须使用 ulimit -v 524288 限制内存为 512MB

# 提取实际命令
if [ -p /dev/stdin ] || [ -f /dev/stdin ]; then
    COMMAND=$(jq -r '.tool_input.command' < /dev/stdin)
else
    COMMAND=$(jq -r '.tool_input.command')  2>/dev/null || COMMAND=""
fi

if [ -z "$COMMAND" ]; then
    exit 0
fi

# 检查是否是构建命令
USE_TIMEOUT=0
if echo "$COMMAND" | grep -qE 'timeout.*'; then
    USE_TIMEOUT=1
fi

# 检查是否是构建命令
IS_BUILD=0
if echo "$COMMAND" | grep -qE '(cargo build|cargo test|cargo check|cargo clippy|rustc)'; then
    IS_BUILD=1
fi

# 检查是否是测试执行命令（运行 lua 测试脚本）
IS_TEST_RUN=0
if echo "$COMMAND" | grep -qE '(\./target/.*/lua .*tests_lua/|lua .*\.lua)'; then
    IS_TEST_RUN=1
fi

# 如果是构建命令，检查是否使用了 systemd-run
if [ "$IS_BUILD" -eq 1 ]; then
    if ! echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
        echo "ERROR: 构建命令必须使用 systemd-run 绕过内存限制" >&2
        echo "请使用: systemd-run --user --wait --collect --pipe --property=LimitAS=infinity <构建命令>" >&2
        echo "例如: systemd-run --user --wait --collect --pipe --property=LimitAS=infinity cargo build" >&2
        exit 2
    fi
fi

# 如果是测试执行命令，检查是否设置了 ulimit
if [ "$IS_TEST_RUN" -eq 1 ]; then
    if echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
        echo "ERROR: 测试命令禁止使用 systemd-run 绕过内存限制" >&2
        echo "请在命令前添加: ulimit -v 524288 && <测试命令>" >&2
        echo "例如: ulimit -v 524288 && ./target/debug/lua tests_lua/test.lua" >&2
        exit 2
    fi
    if ! echo "$COMMAND" | grep -q 'ulimit -v 524288'; then
        echo "ERROR: 测试命令必须使用 ulimit -v 524288 限制内存为 512MB" >&2
        echo "请在命令前添加: ulimit -v 524288 && <测试命令>" >&2
        echo "例如: ulimit -v 524288 && ./target/debug/lua tests_lua/test.lua" >&2
        exit 2
    fi
    if [ "$USE_TIMEOUT" -eq 0 ]; then
        echo "WARNING: 测试命令最好使用 timeout 命令设置超时时间" >&2
        echo "请在命令前添加: timeout 10 <测试命令>" >&2
        echo "例如: timeout 10 ./target/debug/lua tests_lua/test.lua" >&2
        exit 1
    fi
fi

# 校验通过
exit 0
