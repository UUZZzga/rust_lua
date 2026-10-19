--============================================================
-- rc_release_test.lua
-- 用于测试「引用计数(RC)实现的 Lua」的递归释放行为
--
-- 用法:
--   lua rc_release_test.lua [N] [测试名]
--   lua rc_release_test.lua 200000 chain
--   lua rc_release_test.lua 50000          -- 默认跑全部
--
-- 测试名: all | chain | nested | shared | cycle | finalizer
--
-- 关注点:
--   1) 断开长链的根引用时，释放是否会递归深入 C 栈
--      -> 若实现是递归释放，N 足够大时会 stack overflow 崩溃
--   2) 共享引用下释放时机是否正确（引用计数归零才释放）
--   3) 循环引用在纯 RC 下是否泄漏
--   4) __gc 终结器被调用的最大递归深度（用于量化递归程度）
--============================================================

local args  = rawget(_G, "arg") or {}
local N     = tonumber(args[1]) or 100000
local WHICH = args[2] or "all"

local t0 = os.clock()

local function hr(s)
  io.write(("\n===== %s =====\n"):format(s))
end

local function ok(s)
  io.write("  [OK]   ", s, "\n")
end

local function info(s)
  io.write("  [INFO] ", s, "\n")
end

local function gc()
  if collectgarbage then collectgarbage("collect") end
end

--============================================================
-- 测试1: 长单链表 —— 断开 head 触发整条链的级联释放
--        纯递归实现: 递归深度 ≈ N
--============================================================
local function test_chain(n)
  hr("测试1: 单链表级联释放 (n=" .. n .. ")")

  local head = { id = 1 }
  local cur  = head
  for i = 2, n do
    local node = { id = i }
    cur.next = node
    cur = node
  end
  cur = nil
  gc()

  io.write(("  构造完毕，链长 %d，准备断开 head ...\n"):format(n))
  head = nil          -- <-- 触发级联释放
  gc()
  ok("级联释放完成，未崩溃")
end

--============================================================
-- 测试2: 深嵌套表 t = {t}
--============================================================
local function test_nested(n)
  hr("测试2: 深嵌套表释放 (n=" .. n .. ")")

  local t = {}
  for _ = 1, n do
    t = { t }
  end
  gc()

  io.write(("  构造完毕，嵌套深度 %d，准备置空根 ...\n"):format(n))
  t = nil
  gc()
  ok("嵌套释放完成，未崩溃")
end

--============================================================
-- 测试3: 两条链共享同一条长尾链
--        先断 a，尾链应存活；再断 b，尾链才整体释放
--============================================================
local function test_shared(n)
  hr("测试3: 共享尾链的引用计数语义 (n=" .. n .. ")")

  local tail = { id = 0 }
  local cur  = tail
  for i = 1, n do
    local node = { id = i }
    cur.next = node
    cur = node
  end
  cur = nil

  local a = { next = tail }
  local b = { next = tail }
  tail = nil          -- 现在尾链只被 a 和 b 持有
  gc()

  io.write("  尾链被 a、b 同时持有，先断开 a ...\n")
  a = nil
  gc()
  -- 此时尾链引用计数应为 1（仍被 b 持有），不应被释放
  ok("断开 a 后未崩溃（尾链应仍存活，被 b 持有）")

  io.write("  再断开 b，尾链引用计数归零 ...\n")
  b = nil
  gc()
  ok("断开 b 后整条尾链释放完成")
end

--============================================================
-- 测试4: 循环引用
--        纯 RC 无 cycle collector 时会泄漏；有则可回收
--============================================================
local function test_cycle(n)
  hr("测试4: 循环引用 (n=" .. n .. ")")

  local first = { id = 1 }
  local cur   = first
  for i = 2, n do
    local node = { id = i }
    cur.next = node
    cur = node
  end
  cur.next = first          -- 首尾相连成环
  cur, first = nil, nil

  local before = collectgarbage("count")
  gc()
  local after = collectgarbage("count")

  io.write(("  环长 %d，gc 前后内存: %.1f KB -> %.1f KB\n")
           :format(n, before, after))

  if after < before - 1 then
    ok("存在循环回收机制，环已被释放")
  else
    info("纯 RC 实现下循环引用会泄漏（需要 cycle collector / 试运行标记清除）")
  end
end

--============================================================
-- 测试5: __gc 终结器递归深度
--        用 depth 计数器量化释放是「递归」还是「迭代/延迟」
--============================================================
local function test_finalizer(n)
  if _VERSION == "Lua 5.1" then
    hr("测试5: 跳过（Lua 5.1 的表不支持 __gc）")
    return
  end
  hr("测试5: __gc 终结器递归深度 (n=" .. n .. ")")

  local depth, maxdepth = 0, 0
  local mt = {
    __gc = function(_self)
      depth = depth + 1
      if depth > maxdepth then maxdepth = depth end
      depth = depth - 1
    end,
  }

  local head = setmetatable({ id = 1 }, mt)
  local cur  = head
  for i = 2, n do
    local node = setmetatable({ id = i }, mt)
    cur.next = node
    cur = node
  end
  cur = nil
  gc()

  head = nil          -- 触发链式终结
  gc()

  io.write(("  最大终结器递归深度 = %d (链长 %d)\n"):format(maxdepth, n))

  if maxdepth >= n - 2 then
    info("释放是【递归】进行的：递归深度 ≈ 链长，N 再大就会栈溢出")
  elseif maxdepth <= 2 then
    ok("释放是【迭代/延迟】进行的：递归深度为常数")
  else
    info("递归深度介于两者之间，可能存在尾调用优化或分批释放")
  end
end

--============================================================
-- 入口
--============================================================
local tests = {
  chain     = test_chain,
  nested    = test_nested,
  shared    = test_shared,
  cycle     = test_cycle,
  finalizer = test_finalizer,
}

io.write(("[RC 递归释放测试] N = %d, 模式 = %s, %s\n")
         :format(N, WHICH, _VERSION))

if WHICH == "all" then
  test_chain(N)
  test_nested(N)
  test_shared(N)
  test_cycle(N)
  test_finalizer(N)
elseif tests[WHICH] then
  tests[WHICH](N)
else
  io.write("未知测试名: ", WHICH, "\n")
  io.write("可选: all, chain, nested, shared, cycle, finalizer\n")
  os.exit(1)
end

io.write(("\n总耗时 %.3f 秒\n"):format(os.clock() - t0))