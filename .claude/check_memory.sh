#!/bin/bash
# Hook 脚本：校验命令是否符合内存限制规则
# - 构建命令必须使用 systemd-run 绕过内存限制
# - 测试命令必须使用 ulimit -v 524288 限制内存为 512MB
# - shell 脚本（如 _run_test.sh）也会被扫描，防止绕过

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

# ================================================================
# 增强检测：扫描 shell 脚本内容，防止通过脚本绕过内存限制
# ================================================================
# 从命令中提取脚本路径（支持 bash/sh <script> 或直接 ./<script>.sh）
SCRIPT_PATH=""
if echo "$COMMAND" | grep -qE '(^| )((bash|sh|source) +|\./)([^ ]*\.sh)'; then
    # 匹配 bash run.sh, sh run.sh, source run.sh, ./run.sh 等形式
    SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/.*\(bash\|sh\|source\) \+\([^ ]*\.sh\).*/\2/p')
    if [ -z "$SCRIPT_PATH" ]; then
        SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/.*\.\/\([^ ]*\.sh\).*/\1/p')
        if [ -n "$SCRIPT_PATH" ]; then
            SCRIPT_PATH="./$SCRIPT_PATH"
        fi
    fi
fi
# 也匹配直接以 ./xxx.sh 开头的命令
if [ -z "$SCRIPT_PATH" ]; then
    if echo "$COMMAND" | grep -qE '^\./[^ ]+\.sh'; then
        SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/^\(\.\/[^ ]*\.sh\).*/\1/p')
    fi
fi

# 如果提取到脚本路径，尝试解析并读取其内容
if [ -n "$SCRIPT_PATH" ]; then
    # 如果是相对路径，转成绝对路径
    if [[ "$SCRIPT_PATH" != /* ]]; then
        # 从命令中提取 cwd（如果有）
        ABS_SCRIPT_PATH="/media/uuzz/6283521EEF804B69/GameProjects/lua-5.5.0/${SCRIPT_PATH#./}"
    else
        ABS_SCRIPT_PATH="$SCRIPT_PATH"
    fi

    if [ -f "$ABS_SCRIPT_PATH" ]; then
        SCRIPT_CONTENT=$(cat "$ABS_SCRIPT_PATH" 2>/dev/null)

        # 检查脚本中是否包含 lua 测试执行命令
        if echo "$SCRIPT_CONTENT" | grep -qE '(\./target/.*/lua|lua .*\.lua)'; then
            # 脚本中包含测试执行命令，标记为测试运行
            IS_TEST_RUN=1

            # 检查脚本自身是否包含 ulimit
            if ! echo "$SCRIPT_CONTENT" | grep -q 'ulimit -v 524288'; then
                echo "ERROR: 脚本 $SCRIPT_PATH 包含 lua 测试执行命令但未设置内存限制"
                echo "请在脚本内添加: ulimit -v 524288"
                echo "或在运行脚本时使用: ulimit -v 524288 && $COMMAND"
                exit 2
            fi

            # 检查脚本自身是否包含 timeout
            if ! echo "$SCRIPT_CONTENT" | grep -q 'timeout'; then
                echo "WARNING: 脚本 $SCRIPT_PATH 包含 lua 测试执行命令但未设置 timeout"
                echo "建议在脚本内使用 timeout 命令包装测试执行"
                exit 1
            fi
        fi

        # 检查脚本中是否包含构建命令
        if echo "$SCRIPT_CONTENT" | grep -qE '(cargo build|cargo test|cargo check|cargo clippy|rustc)'; then
            if ! echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
                if ! echo "$SCRIPT_CONTENT" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
                    echo "ERROR: 脚本 $SCRIPT_PATH 包含构建命令但未使用 systemd-run 绕过内存限制"
                    echo "请在脚本内使用 systemd-run --user --wait --collect --pipe --property=LimitAS=infinity 包装构建命令"
                    exit 2
                fi
            fi
        fi
    fi
fi

# 如果是构建命令，检查是否使用了 systemd-run
if [ "$IS_BUILD" -eq 1 ]; then
    if ! echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
        echo "ERROR: 构建命令必须使用 systemd-run 绕过内存限制"
        echo "请使用: systemd-run --user --wait --collect --pipe --property=LimitAS=infinity <构建命令>"
        echo "例如: systemd-run --user --wait --collect --pipe --property=LimitAS=infinity cargo build"
        exit 2
    fi
fi

# 如果是测试执行命令，检查是否设置了 ulimit
if [ "$IS_TEST_RUN" -eq 1 ]; then
    if echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
        echo "ERROR: 测试命令禁止使用 systemd-run 绕过内存限制"
        echo "请在命令前添加: ulimit -v 524288 && <测试命令>"
        echo "例如: ulimit -v 524288 && ./target/debug/lua tests_lua/test.lua"
        exit 2
    fi
    if ! echo "$COMMAND" | grep -q 'ulimit -v 524288'; then
        echo "ERROR: 测试命令必须使用 ulimit -v 524288 限制内存为 512MB"
        echo "请在命令前添加: ulimit -v 524288 && <测试命令>"
        echo "例如: ulimit -v 524288 && ./target/debug/lua tests_lua/test.lua"
        exit 2
    fi
    if [ "$USE_TIMEOUT" -eq 0 ]; then
        echo "WARNING: 测试命令最好使用 timeout 命令设置超时时间"
        echo "请在命令前添加: timeout 10 <测试命令>"
        echo "例如: timeout 10 ./target/debug/lua tests_lua/test.lua"
        exit 1
    fi
fi

# 校验通过
exit 0
