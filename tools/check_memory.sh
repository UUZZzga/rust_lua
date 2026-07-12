#!/bin/bash
# Hook 脚本：校验命令是否符合内存限制规则
# - 构建命令必须使用 systemd-run 绕过内存限制
# - 测试命令必须使用 systemd-run --user --property=LimitAS=204800 限制内存为 200MB
# - shell/Python 脚本也会被扫描内容，防止绕过

# 提取实际命令（支持 pipe 输入和交互式终端输入）
if [ -t 0 ]; then
    # 交互式终端：读一行
    read -r STDIN_DATA
else
    # Pipe/重定向：读全部
    STDIN_DATA=$(cat)
fi
COMMAND=$(echo "$STDIN_DATA" | jq -r '.tool_input.command' 2>/dev/null || echo "")

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
# 增强检测：扫描脚本内容，防止通过脚本绕过内存限制
# 支持：shell 脚本 (.sh) 和 Python 脚本 (.py)
# ================================================================
PROJECT_ROOT="/media/uuzz/6283521EEF804B69/GameProjects/lua-5.5.0"

# 从命令中提取脚本路径
SCRIPT_PATH=""
# 匹配 bash/sh/source <script>
if echo "$COMMAND" | grep -qE '(^| )((bash|sh|source) +)([^ ]+\.(sh|py))'; then
    SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/.*\(bash\|sh\|source\) \+\([^ ]*\.\(sh\|py\)\).*/\2/p')
fi
# 匹配 python3/python <script>
if [ -z "$SCRIPT_PATH" ]; then
    if echo "$COMMAND" | grep -qE '(^| )(python|python3) +[^ ]+\.py'; then
        SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/.*\(python\|python3\) \+\([^ ]*\.py\).*/\2/p')
    fi
fi
# 匹配直接以 ./xxx.sh 或 ./xxx.py 开头的命令
if [ -z "$SCRIPT_PATH" ]; then
    if echo "$COMMAND" | grep -qE '^\./[^ ]+\.(sh|py)'; then
        SCRIPT_PATH=$(echo "$COMMAND" | sed -n 's/^\(\.\/[^ ]*\.\(sh\|py\)\).*/\1/p')
    fi
fi

# 如果提取到脚本路径，尝试解析并读取其内容
if [ -n "$SCRIPT_PATH" ]; then
    # 如果是相对路径，转成绝对路径
    if [[ "$SCRIPT_PATH" != /* ]]; then
        ABS_SCRIPT_PATH="${PROJECT_ROOT}/${SCRIPT_PATH#./}"
    else
        ABS_SCRIPT_PATH="$SCRIPT_PATH"
    fi

    if [ -f "$ABS_SCRIPT_PATH" ]; then
        SCRIPT_CONTENT=$(cat "$ABS_SCRIPT_PATH" 2>/dev/null)
        SCRIPT_EXT="${SCRIPT_PATH##*.}"

        # --- 检测测试执行命令（适用于所有脚本类型） ---
        HAS_TEST_CMD=0

        # 检测 ./target/.../lua 或 lua xxx.lua 调用
        if echo "$SCRIPT_CONTENT" | grep -qE '((\./)?target/[^ ]*/lua|lua .*\.lua)'; then
            HAS_TEST_CMD=1
        fi

        if [ "$HAS_TEST_CMD" -eq 1 ]; then
            IS_TEST_RUN=1

            # shell 脚本检查 systemd-run 内存限制
            if [ "$SCRIPT_EXT" = "sh" ]; then
                if ! echo "$SCRIPT_CONTENT" | grep -q 'systemd-run.*--property=LimitAS=204800'; then
                    echo "ERROR: 脚本 $SCRIPT_PATH 包含 lua 测试执行命令但未设置内存限制"
                    echo "请在脚本内使用: systemd-run --user --wait --collect --pipe --property=LimitAS=204800"
                    echo "或在运行脚本时使用: systemd-run --user --wait --collect --pipe --property=LimitAS=204800 && <命令>"
                    exit 2
                fi
                if ! echo "$SCRIPT_CONTENT" | grep -q 'timeout'; then
                    echo "WARNING: 脚本 $SCRIPT_PATH 包含 lua 测试执行命令但未设置 timeout"
                    echo "建议在脚本内使用 timeout 命令包装测试执行"
                    exit 1
                fi
            fi

            # Python 脚本：检查是否使用了 systemd-run 限制内存
            if [ "$SCRIPT_EXT" = "py" ]; then
                # 检查是否通过 subprocess 调用时使用了 systemd-run
                HAS_MEM_LIMIT=0

                # 方法1：脚本自身通过 resource.setrlimit 限制
                if echo "$SCRIPT_CONTENT" | grep -q 'resource\.setrlimit'; then
                    HAS_MEM_LIMIT=1
                fi
                # 方法2：通过 systemd-run 包装命令
                if echo "$SCRIPT_CONTENT" | grep -qE 'systemd-run.*--property=LimitAS=204800'; then
                    HAS_MEM_LIMIT=1
                fi
                # 方法3：subprocess 调用时 shell=True 且命令中包含 systemd-run
                if echo "$SCRIPT_CONTENT" | grep -qE 'shell=True.*systemd-run|systemd-run.*shell=True'; then
                    HAS_MEM_LIMIT=1
                fi

                if [ "$HAS_MEM_LIMIT" -eq 0 ]; then
                    echo "ERROR: Python 脚本 $SCRIPT_PATH 包含 lua 测试执行但未设置内存限制"
                    echo "请在 Python 脚本中使用 resource.setrlimit 限制内存"
                    echo "或在 subprocess 调用中使用 'systemd-run --user --wait --collect --pipe --property=LimitAS=204800 && <命令>'"
                    exit 2
                fi
            fi
        fi

        # --- 检测构建命令 ---
        HAS_BUILD_CMD=0
        if echo "$SCRIPT_CONTENT" | grep -qE '(cargo build|cargo test|cargo check|cargo clippy|rustc)'; then
            HAS_BUILD_CMD=1
        fi

        if [ "$HAS_BUILD_CMD" -eq 1 ]; then
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

# 如果是测试执行命令，检查是否设置了 systemd-run 内存限制
if [ "$IS_TEST_RUN" -eq 1 ]; then
    if echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=infinity'; then
        echo "ERROR: 测试命令禁止使用 systemd-run 绕过内存限制"
        echo "请使用: systemd-run --user --wait --collect --pipe --property=LimitAS=204800 <测试命令>"
        echo "例如: systemd-run --user --wait --collect --pipe --property=LimitAS=204800 ./target/debug/lua tests_lua/test.lua"
        exit 2
    fi
    if ! echo "$COMMAND" | grep -q 'systemd-run.*--property=LimitAS=204800'; then
        echo "ERROR: 测试命令必须使用 systemd-run --user --property=LimitAS=204800 限制内存为 200MB"
        echo "请在命令前添加: systemd-run --user --wait --collect --pipe --property=LimitAS=204800 && <测试命令>"
        echo "例如: systemd-run --user --wait --collect --pipe --property=LimitAS=204800 ./target/debug/lua tests_lua/test.lua"
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
