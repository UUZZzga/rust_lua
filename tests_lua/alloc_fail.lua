-- $Id: alloc_fail.lua $
-- 内存分配失败处理测试 —— 对应 C 的 tests_lua/memerr.lua（C 版依赖测试库 T / ltests，
-- Rust 实现不编译 ltests，故改为在**受限地址空间**下真实耗尽内存来验证同一语义）。
--
-- 运行要求：进程地址空间必须受限（本地/CI 统一 200MB）：
--   systemd-run --user --wait --collect --pipe --property=LimitAS=204800 \
--       ./target/release/lua tests_lua/alloc_fail.lua
-- `tools/verify_alloc_fail.sh` 已封装该限制（`tools/verify.sh` 会调用它），
-- 无法施加限制的环境（无 user systemd 且无 ulimit -v）会跳过而不是无限运行。
--
-- 验证目标（对应 lmem.c 的 luaM_realloc_/tryagain/luaM_error 与 ldo.c 的 LUA_ERRMEM）：
--   1) 内存耗尽时不得崩溃：不 abort、不被信号杀死，而是先做紧急全量 GC 再重试分配；
--   2) 重试仍失败时抛**可捕获**的内存错误，消息取自状态创建时预注册的 memerrmsg
--      （"not enough memory"，传播过程零分配）；
--   3) 错误之后解释器仍完全可用（栈 / 全局表 / GC 均未被破坏）。

local MEMERR = "not enough memory"

local function checkerr (msg, f, ...)
  local stat, err = pcall(f, ...)
  assert(not stat, "预期分配失败但没有失败")
  local s = tostring(err)
  assert(string.find(s, msg, 1, true), "错误消息不匹配: " .. s)
  return err
end

print("testing memory-allocation errors (requires a limited address space)")

-- {==================================================================
-- 1) 栈扩容（深递归）—— 说明：C 栈上限（"C stack overflow"）在 *错误处理自身*
--    超出上限时按 LUA_ERRERR 处理，可能不被内层 pcall 捕获（C 同语义），
--    与本测试关注的「分配失败」无关，故此处只做浅递归的正常性检查。
--    深递归路径的分配失败覆盖见 tests_rs/alloc_failure_tests.rs 与 CLAUDE.md 的
--    「尚未纳入漏斗」清单。
-- ===================================================================}
do
  local function deep (n)
    if n > 0 then return deep(n - 1) + 1 end
    return 0
  end
  assert(deep(100) == 100)
  -- 递归上限（本实现约 200 层）触发时必须是**可捕获**的错误，而不是崩溃
  local ok, e = pcall(deep, 100000)
  assert(not ok, "预期递归上限触发")
  assert(string.find(tostring(e), "stack overflow", 1, true), tostring(e))
end

-- {==================================================================
-- 2) 真实耗尽：不可回收的增长必须报出可捕获的内存错误，而不是崩溃
-- ===================================================================}
do
  local err = checkerr(MEMERR, function ()
    local t = {}
    -- t 一直可达 → 紧急 GC 无法回收 → 重试必然失败 → 内存错误
    for i = 1, math.maxinteger do
      t[i] = {}
    end
  end)
  assert(string.find(tostring(err), MEMERR, 1, true))
end

-- 错误之后：GC 可回收，解释器继续工作（证明错误路径没有破坏状态）
collectgarbage("collect")
do
  local sum = 0
  for i = 1, 1000 do sum = sum + i end
  assert(sum == 500500)

  local t = {}
  for i = 1, 100 do t[i] = i end
  assert(#t == 100 and t[100] == 100)

  local s = string.rep("a", 1000)
  assert(#s == 1000)
end

-- {==================================================================
-- 3) 可回收压力：产生大量不可达垃圾并持续分配
--    （分配失败时紧急 GC 必须能腾出内存使重试成功，不得报错）
-- ===================================================================}
do
  collectgarbage("collect")
  local before = collectgarbage("count")
  local n = 0
  for i = 1, 300000 do
    -- 每轮丢弃：垃圾可回收；若分配失败则紧急 GC 后重试应成功
    local tmp = { i, i + 1 }
    n = n + tmp[2] - tmp[1]
  end
  assert(n == 300000)
  assert(collectgarbage("count") < before + 20000, "垃圾未能被回收（内存增长过大）")
end

-- 结束前回到干净状态
collectgarbage("collect")

print("Ok")
