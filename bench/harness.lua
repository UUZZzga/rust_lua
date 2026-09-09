-- bench/harness.lua — ci_bench 公共驱动; 用法:
--   lua bench/harness.lua <quick|full> <基准文件> [<基准文件> ...]
local scale = arg[1] or "quick"
if scale ~= "quick" and scale ~= "full" then
    io.stderr:write("harness.lua: 无效规模 '" .. tostring(scale) .. "' (期望 quick|full)\n")
    os.exit(2)
end
if #arg < 2 then
    io.stderr:write("harness.lua: 缺少基准文件参数\n")
    os.exit(2)
end
print(string.format("=== ci_bench (%s) ===", scale))
print(string.format("Lua 版本: %s", _VERSION))
for i = 2, #arg do
    local item = assert(dofile(arg[i]))
    collectgarbage("collect")
    collectgarbage("collect")
    local n = (scale == "full") and item.full or item.quick
    local t0 = os.clock()
    item.fn(n)
    local dt = os.clock() - t0
    print(string.format("  >> %s(N=%d): %.4f", item.name, n, dt))
end
print("=== ci_bench 完成 ===")
