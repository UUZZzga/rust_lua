-- bench/harness.lua — ci_bench 公共驱动; 用法:
--   lua bench/harness.lua <quick|full> <基准文件> [<基准文件> ...]
--
-- 多轮采样: 环境变量 BENCH_REPEAT (默认 1, CI full 建议 3)。
-- 每项基准重复执行取【最小值】— 最小值受系统噪声干扰最少 (热循环被中断/
-- 调度抢占只会让某轮变慢, 不会让某轮无故变快), 是性能基准的标准口径。
-- 输出每轮原始值 + min 行 (无 BENCH_REPEAT 时格式与旧版完全一致)。
local scale = arg[1] or "quick"
if scale ~= "quick" and scale ~= "full" then
    io.stderr:write("harness.lua: 无效规模 '" .. tostring(scale) .. "' (期望 quick|full)\n")
    os.exit(2)
end
if #arg < 2 then
    io.stderr:write("harness.lua: 缺少基准文件参数\n")
    os.exit(2)
end
local repeat_n = tonumber(os.getenv("BENCH_REPEAT") or "1") or 1
if repeat_n < 1 then repeat_n = 1 end
print(string.format("=== ci_bench (%s) ===", scale))
print(string.format("Lua 版本: %s", _VERSION))
if repeat_n > 1 then
    print(string.format("采样: %d 轮取最小值", repeat_n))
end
for i = 2, #arg do
    local item = assert(dofile(arg[i]))
    local n = (scale == "full") and item.full or item.quick
    local best
    for r = 1, repeat_n do
        collectgarbage("collect")
        collectgarbage("collect")
        local t0 = os.clock()
        item.fn(n)
        local dt = os.clock() - t0
        if best == nil or dt < best then best = dt end
        if repeat_n > 1 then
            print(string.format("  .. %s N=%d r%d: %.4f", item.name, n, r, dt))
        end
    end
    print(string.format("  >> %s(N=%d): %.4f", item.name, n, best))
end
print("=== ci_bench 完成 ===")
