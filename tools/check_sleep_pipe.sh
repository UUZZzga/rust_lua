#!/bin/bash
# Hook 脚本：禁止使用 'sleep N | exe' 形式的命令
#
# 原因：
#   管道中 shell 会等待所有命令结束。exe 退出后 sleep 仍在运行，
#   整个管道会阻塞至 sleep 计时结束，造成数倍乃至数百倍的无效等待。
#   例如 'sleep 300 | lua all.lua'，lua 即使 1 秒结束，整体仍会等 300 秒。
#
# 替代方案（按用途选择）：
#   - 不需要 stdin 数据，仅要求 stdin 不可 seek（files.lua:88 场景）：
#       printf '' | exe              # 空管道，立即 EOF，stdin 不可 seek
#   - 需要少量数据输入：
#       echo "..." | exe
#       printf '%s\n' "..." | exe
#   - 需要从文件读取：
#       exe < file
#   - 不需要任何 stdin：
#       exe < /dev/null
#   - 需要超时控制：
#       timeout N exe < /dev/null

# 提取实际命令（支持 pipe 输入和交互式终端输入）
if [ -t 0 ]; then
    read -r STDIN_DATA
else
    STDIN_DATA=$(cat)
fi
COMMAND=$(echo "$STDIN_DATA" | jq -r '.tool_input.command' 2>/dev/null || echo "")

if [ -z "$COMMAND" ]; then
    exit 0
fi

# 检测 'sleep N | ...' 形式
# 匹配规则：
#   sleep 关键字 + 空白 + 数字（可带单位 s/m/h/d）+ 可选空白 + 管道符 |
# 单位大小写均匹配（S/M/H/D 也可，虽然非标准但 sleep 拒绝时仍会卡住管道）
#
# 已知限制：grep 无法识别 shell 引号边界，因此 `echo 'sleep 5 | cat'` 这类
# 字符串字面量也会被误判。设计取向为"宁可误判不可漏判"——若确需 echo 含
# `sleep N |` 字面量，改用 `printf '%s\n' 'sleep 5 | cat'` 即可绕过。
if echo "$COMMAND" | grep -qE 'sleep[[:space:]]+[0-9]+[smhdSMHD]?[[:space:]]*\|'; then
    echo "ERROR: 禁止使用 'sleep N | exe' 形式的命令"
    echo ""
    echo "原因：sleep 不会因 exe 结束而提前退出，整个管道会阻塞至 sleep 计时结束，"
    echo "      造成大量无效等待时间（如 lua 1 秒结束，整体仍要等 300 秒）。"
    echo ""
    echo "替代方案："
    echo "  - 仅需 stdin 不可 seek（files.lua:88 场景）：printf '' | exe"
    echo "  - 需少量数据输入：echo \"...\" | exe 或 printf '%s\n' \"...\" | exe"
    echo "  - 需从文件读取：exe < file"
    echo "  - 不需要 stdin：exe < /dev/null"
    echo "  - 需超时控制：timeout N exe < /dev/null"
    exit 2
fi

# 校验通过
exit 0
