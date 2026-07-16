-- gc_linkedlist_test.lua
-- 测试: Rust GC 释放长链表时是否会栈溢出

local function test_linkedlist(n)
    collectgarbage("collect")
    local mem_before = collectgarbage("count")

    -- 创建单向链表: head -> n1 -> n2 -> ... -> n
    local head = {}
    local cur = head
    for i = 1, n do
        cur.next = {}
        cur = cur.next
    end

    local mem_after_create = collectgarbage("count")
    print(string.format("  创建 %d 节点链表: %.1f KB -> %.1f KB (+%.1f KB)",
        n, mem_before, mem_after_create, mem_after_create - mem_before))

    -- 释放链表: head = nil 后, Rc 递归 drop
    -- 如果链表很长, 递归 drop 可能栈溢出
    head = nil
    cur = nil

    -- 强制 GC
    collectgarbage("collect")
    local mem_after_gc = collectgarbage("count")
    print(string.format("  释放+GC后: %.1f KB (回收 %.1f KB)",
        mem_after_gc, mem_after_create - mem_after_gc))
    print(string.format("  剩余增量: %.1f KB", mem_after_gc - mem_before))
end

local function test_circular(n)
    collectgarbage("collect")
    local mem_before = collectgarbage("count")

    -- 创建循环引用链表: h0 -> h1 -> ... -> h(n-1) -> h0
    local nodes = {}
    for i = 1, n do
        nodes[i] = {}
    end
    for i = 1, n do
        nodes[i].next = nodes[i % n + 1]
    end

    local mem_after_create = collectgarbage("count")
    print(string.format("  创建 %d 节点循环链表: %.1f KB -> %.1f KB (+%.1f KB)",
        n, mem_before, mem_after_create, mem_after_create - mem_before))

    -- 断开外部引用
    nodes = nil

    -- 强制 GC — 循环引用的 Rc 计数不为 0, GC 需要显式释放
    collectgarbage("collect")
    local mem_after_gc = collectgarbage("count")
    print(string.format("  释放+GC后: %.1f KB (回收 %.1f KB)",
        mem_after_gc, mem_after_create - mem_after_gc))
    print(string.format("  剩余增量: %.1f KB", mem_after_gc - mem_before))
end

print("=== 链表 GC 释放测试 ===")
print(string.format("Lua 版本: %s\n", _VERSION))

print("[1] 单向链表 (递归 drop 风险)")
for _, n in ipairs({1000, 10000, 100000, 500000}) do
    print(string.format("\n  --- 链表长度 %d ---", n))
    local ok, err = pcall(test_linkedlist, n)
    if not ok then
        print(string.format("  失败: %s", err))
        break
    end
end

print("\n[2] 循环引用链表 (Rc 计数不为0, GC 需显式释放)")
for _, n in ipairs({1000, 10000, 100000}) do
    print(string.format("\n  --- 循环链表长度 %d ---", n))
    local ok, err = pcall(test_circular, n)
    if not ok then
        print(string.format("  失败: %s", err))
        break
    end
end

print("\n=== 测试完成 ===")
