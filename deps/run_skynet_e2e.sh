#!/bin/bash
# run_skynet_e2e.sh - 端到端测试：启动 skynet 服务端 + 运行 client.lua 客户端
#
# 对应 Skynet README.md 第 33-34 行的测试步骤：
#   ./skynet examples/config            # 启动 skynet 节点（Gate server，链接 lua-rs）
#   ./3rd/lua/lua examples/client.lua   # 启动客户端（用 C lua，因 lpeg.so 与 lua-rs 有兼容性问题）
#
# 服务端 skynet 二进制链接了 liblua_rs.a（Rust 实现），验证 lua-rs 的 C ABI 兼容性。
# 客户端使用 C lua (build/lua)，因为 client.lua 依赖 lpeg.so 进行 sproto 协议解析，
# 而 lpeg 的 pattern 编译触发了 lua-rs 的 userdata/GC 兼容性问题（segfault）。
# 客户端用 C lua 不影响对服务端（lua-rs）的验证，因为网络协议是跨实现的。
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SKYNET_DIR="$SCRIPT_DIR/src/skynet"
# 服务端用 lua-rs（skynet 二进制已链接 liblua_rs.a）
# 客户端用 C lua（因 lpeg.so 兼容性问题）
LUA_C_BIN="$PROJECT_ROOT/build/lua"

cd "$SKYNET_DIR"

# 清理旧进程和日志
pkill -9 -f "skynet examples/config" 2>/dev/null || true
sleep 1
rm -f /tmp/skynet_e2e_server.log /tmp/skynet_e2e_client.log

# 检查 C lua 二进制
if [[ ! -x "$LUA_C_BIN" ]]; then
    echo "[ERROR] C lua 二进制不存在: $LUA_C_BIN"
    echo "  请先构建: cd $PROJECT_ROOT && cmake -B build && cmake --build build"
    exit 2
fi

echo "[1/4] 启动 skynet 服务端 (examples/config, 链接 lua-rs)..."
# 创建命名管道保持 stdin 打开（console 服务需要 stdin）
STDIN_FIFO=$(mktemp -u /tmp/skynet_stdin_XXXXXX)
mkfifo "$STDIN_FIFO"
sleep 90 > "$STDIN_FIFO" &
SLEEP_PID=$!
timeout 90 ./skynet examples/config > /tmp/skynet_e2e_server.log 2>&1 < "$STDIN_FIFO" &
SERVER_PID=$!
echo "  server_pid=$SERVER_PID"

# 等待 port 8888 就绪（最多 60 秒）
echo "[2/4] 等待 port 8888 就绪..."
READY=0
for i in $(seq 1 60); do
    if ss -tln 2>&1 | grep -q ":8888"; then
        echo "  port 8888 ready after ${i}s"
        READY=1
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "  [ERROR] 服务端进程已退出"
        break
    fi
    sleep 1
done

if [[ $READY -ne 1 ]]; then
    echo "  [ERROR] port 8888 未就绪"
    echo "--- server log ---"
    tail -30 /tmp/skynet_e2e_server.log
    kill "$SERVER_PID" 2>/dev/null || true
    kill "$SLEEP_PID" 2>/dev/null || true
    rm -f "$STDIN_FIFO"
    exit 1
fi

echo "--- server log (last 10 lines) ---"
tail -10 /tmp/skynet_e2e_server.log

# 确认服务端进程仍存活
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "  [ERROR] 服务端进程在客户端运行前已退出"
    rm -f "$STDIN_FIFO"
    exit 1
fi

echo "[3/4] 运行 client.lua (C lua, 发送 handshake + set + hello)..."
# 客户端先自动发送 handshake 和 set，然后从 stdin 读取命令
# 重要：client.so 的 luaopen_client_socket 会启动 readline_stdin 后台线程，
# 该线程在 stdin EOF 时调用 exit(1) 直接终止进程。
# 因此不能用 printf | lua 管道（管道立即 EOF 导致 exit(1)），
# 也不能用 ( printf > FIFO; sleep ) 形式（printf 退出后 FIFO 写端关闭导致 EOF），
# 必须用子 shell 级别重定向 ( cmd1; sleep ) > FIFO，让 sleep 也持有 FIFO 写端。
#
# quit 必须与 hello 分开发送（中间 sleep 等待响应）：
# 客户端主循环每次 readstdin 取一行即发送，若 hello 和 quit 同时入队，
# quit 会紧跟 hello 发出，服务端处理 quit 后 KILL self 断连，
# hello 的响应 (RESPONSE 3 result=world) 丢失导致测试失败。
CLIENT_STDIN_FIFO=$(mktemp -u /tmp/skynet_client_stdin_XXXXXX)
mkfifo "$CLIENT_STDIN_FIFO"
# 后台子 shell：先写 hello，等 2s 让客户端发出 get 请求并收到响应，
# 再写 quit；sleep 保持 FIFO 写端打开。
( printf 'hello\n'; sleep 2; printf 'quit\n'; sleep 15 ) > "$CLIENT_STDIN_FIFO" &
CLIENT_WRITER_PID=$!
cd "$SKYNET_DIR"
timeout 15 "$LUA_C_BIN" examples/client.lua < "$CLIENT_STDIN_FIFO" > /tmp/skynet_e2e_client.log 2>&1
CLIENT_RC=$?
cd "$PROJECT_ROOT"
kill "$CLIENT_WRITER_PID" 2>/dev/null || true
rm -f "$CLIENT_STDIN_FIFO"

echo "--- client log ---"
cat /tmp/skynet_e2e_client.log
echo "  client exit code: $CLIENT_RC"
echo "--- server log after client ---"
tail -10 /tmp/skynet_e2e_server.log

echo "[4/4] 关闭服务端..."
kill "$SERVER_PID" 2>/dev/null || true
kill "$SLEEP_PID" 2>/dev/null || true
pkill -9 -f "skynet examples/config" 2>/dev/null || true
rm -f "$STDIN_FIFO"

# 判定结果
echo "========================================"
# 客户端退出码 124 是 timeout 超时（readline_stdin 后台线程在 fgets 阻塞，
# 进程不主动退出），这是 FIFO 方案的预期行为，不算失败。
# 关键判定标准：客户端日志中包含 "RESPONSE" 且有 "result" 字段（收到 simpledb 响应）
if grep -q "RESPONSE" /tmp/skynet_e2e_client.log && grep -q "result" /tmp/skynet_e2e_client.log; then
    echo "[PASS] skynet 端到端测试通过"
    echo "  - 服务端启动成功 (port 8888, 使用 lua-rs)"
    echo "  - 客户端连接成功并收到响应 (使用 C lua)"
    # 显示响应内容
    echo "--- 响应内容 ---"
    grep -E "Request:|RESPONSE|msg|result" /tmp/skynet_e2e_client.log | sed 's/^/  /'
    exit 0
else
    echo "[FAIL] 客户端未收到完整服务端响应"
    echo "  期望: RESPONSE 行 + result 字段"
    exit 1
fi
