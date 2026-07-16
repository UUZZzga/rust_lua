-- gc_linkedlist_diag.lua
-- 精确诊断: Rust 实现释放长链表时的行为

local function diag(n)
    collectgarbage("collect")
    collectgarbage("stop")  -- 停止自动 GC, 排除干扰
    local m0 = collectgarbage("count")
    print(string.format("  初始内存: %.1f KB", m0))

    -- 创建单向链表
    local head = {}
    local cur = head
    for i = 1, n do
        cur.next = {}
        cur = cur.next
    end
    local m1 = collectgarbage("count")
    print(string.format("  创建 %d 节点后: %.1f KB (+%.1f KB)", n, m1, m1 - m0))

    -- 释放链表
    head = nil
    cur = nil
    local m2 = collectgarbage("count")
    print(string.format("  head=nil 后: %.1f KB (drop释放 %.1f KB)", m2, m1 - m2))

    -- 手动 GC
    collectgarbage("restart")
    collectgarbage("collect")
    local m3 = collectgarbage("count")
    print(string.format("  GC 后: %.1f KB (GC释放 %.1f KB)", m3, m2 - m3))
    print(string.format("  总剩余增量: %.1f KB", m3 - m0))
end

print("=== 链表释放诊断 ===")
print(string.format("Lua 版本: %s\n", _VERSION))

for _, n in ipairs({100, 1000, 10000, 100000}) do
    print(string.format("--- 链表长度 %d ---", n))
    local ok, err = pcall(diag, n)
    if not ok then
        print(string.format("  失败: %s", err))
        break
    end
    print()
end

print("=== 诊断完成 ===")
