-- gc_bench.lua
-- GC 性能测试: 吞吐量、暂停时间、内存占用
-- 同时兼容 C 实现与 Rust 实现
--
-- 所有汇总行以 ">> " 开头, 格式为 ">> 指标: 值", 便于脚本提取对比。
--
-- 用法:
--   lua gc_bench.lua [N] [ROUNDS] [STRIDE_N] [STEP_SIZE] [CONFIG_COUNT]
--
-- 命令行参数 (均可省略, 使用默认值):
--   N            主测试对象数量         默认 200000
--   ROUNDS       每项测试重复轮数       默认 5
--   STRIDE_N     字符串/闭包测试数量    默认 50000
--   STEP_SIZE    增量GC单步的stepsize   默认 256
--   CONFIG_COUNT 配置工作集对象数量     默认 5000
--
-- 示例:
--   lua gc_bench.lua                       -- 默认压力
--   lua gc_bench.lua 1000000 10            -- 100万对象, 10轮
--   lua gc_bench.lua 2000000 10 200000 1024 20000  -- 高压力+大配置集

local function parse_arg(idx, default)
    local v = arg and arg[idx]
    if v == nil then return default end
    local n = tonumber(v)
    return (n and n > 0) and n or default
end

local N           = parse_arg(1, 200000)   -- 主测试对象数量
local ROUNDS      = parse_arg(2, 5)         -- 重复轮数
local STRIDE_N    = parse_arg(3, 50000)     -- 字符串/闭包测试数量
local STEP_SIZE   = parse_arg(4, 256)       -- 增量GC单步stepsize
local CONFIG_COUNT = parse_arg(5, 5000)     -- 配置工作集对象数量

---------------------------------------------------------------------
-- 辅助函数
---------------------------------------------------------------------
local function KB(v) return string.format("%.2f KB", v) end
local function SEC(v) return string.format("%.4f", v) end

local function section(title)
    print(string.format("\n=== %s ===", title))
end

local function summary(metric, value)
    print(string.format("  >> %s: %s", metric, value))
end

-- 计算分位数 (输入需已排序)
local function percentile(sorted, p)
    local n = #sorted
    if n == 0 then return 0 end
    local idx = math.ceil(n * p)
    if idx < 1 then idx = 1 end
    if idx > n then idx = n end
    return sorted[idx]
end

-- 构建模拟游戏配置对象 (长期存活, 嵌套结构)
local function make_config(i)
    return {
        id = i,
        name = "config_" .. i,
        type = i % 100,
        attrs = {
            hp = i * 10, mp = i * 5,
            atk = i * 2, def = i,
            spd = i % 100,
        },
        skills = { "fire", "ice", "thunder" },
        tags = { i % 2 == 0, i % 3 == 0, i % 5 == 0 },
        meta = { version = 1, author = "system" },
    }
end

-- 构建模拟玩家登录临时对象 (短命, 有引用关系)
local function make_player(wave, p)
    local buf = {}
    for b = 1, 5 do
        buf[b] = { id = b, val = p * b, str = string.rep("x", 16) }
    end
    return {
        name = "p_" .. wave .. "_" .. p,
        pos = { x = p, y = p * 2 },
        buf = buf,
    }
end

---------------------------------------------------------------------
-- 测试 1: GC 吞吐量 (table 对象/秒)
--   停止自动 GC，分配 N 个临时对象，测量一次完整 GC 的耗时。
---------------------------------------------------------------------
local function bench_throughput()
    section("1. GC 吞吐量 (table 对象/秒)")
    collectgarbage("collect")
    collectgarbage("stop")

    local sum = 0
    for r = 1, ROUNDS do
        for i = 1, N do
            local _ = { i, i * 2, i * 3 }
        end
        local t0 = os.clock()
        collectgarbage("collect")
        local t1 = os.clock()
        local dt  = t1 - t0
        local thru = N / dt
        sum = sum + thru
        print(string.format("  round %d: GC %.4fs, 吞吐 %.0f obj/s", r, dt, thru))
    end
    summary("table吞吐(obj/s)", string.format("%.0f", sum / ROUNDS))
    collectgarbage("restart")
end

---------------------------------------------------------------------
-- 测试 2: 完整 GC 暂停时间
--   建立存活工作集 + 临时垃圾，测量单次完整 GC 的暂停时间。
---------------------------------------------------------------------
local function bench_full_gc_pause()
    section("2. 完整 GC 暂停时间")
    collectgarbage("collect")
    collectgarbage("stop")

    local max_p, total, cnt = 0, 0, 0
    for r = 1, ROUNDS do
        local keep = {}
        for i = 1, N do keep[i] = { i, i * 2 } end
        for i = 1, N do local _ = { i, i * 2, i * 3, i * 4 } end

        local t0 = os.clock()
        collectgarbage("collect")
        local t1 = os.clock()
        local p = t1 - t0
        if p > max_p then max_p = p end
        total, cnt = total + p, cnt + 1
        print(string.format("  round %d: 暂停 %.4fs", r, p))
    end
    summary("完整GC最大暂停(s)", SEC(max_p))
    summary("完整GC平均暂停(s)", SEC(total / cnt))
    collectgarbage("restart")
end

---------------------------------------------------------------------
-- 测试 3: 增量 GC 单步暂停时间
--   停止自动 GC，用大 stepsize 手动驱动，记录每步耗时。
---------------------------------------------------------------------
local function bench_step_pause()
    section(string.format("3. 增量 GC 单步暂停时间 (stepsize=%d)", STEP_SIZE))
    collectgarbage("collect")
    collectgarbage("stop")

    local keep = {}
    for i = 1, N do keep[i] = { i, i * 2 } end
    for i = 1, N do local _ = { i, i * 2, i * 3 } end

    local max_s, total, cnt = 0, 0, 0
    local done = false
    while not done and cnt < 50000 do
        local t0 = os.clock()
        done = collectgarbage("step", STEP_SIZE)
        local t1 = os.clock()
        local dt = t1 - t0
        if dt > max_s then max_s = dt end
        total, cnt = total + dt, cnt + 1
    end
    summary("增量步数", cnt)
    summary("增量最大单步(s)", SEC(max_s))
    summary("增量平均单步(s)", cnt > 0 and SEC(total / cnt) or "N/A")
    collectgarbage("restart")
end

---------------------------------------------------------------------
-- 测试 4: 内存占用
--   测量基础内存、保留 N 对象峰值、释放一半、全部释放。
---------------------------------------------------------------------
local function bench_memory()
    section("4. 内存占用")
    collectgarbage("collect")
    local base = collectgarbage("count")
    summary("基础内存(KB)", KB(base))

    local keep = {}
    for i = 1, N do keep[i] = { i, i * 2, i * 3 } end
    collectgarbage("collect")
    local peak = collectgarbage("count")
    summary("峰值内存(KB)", KB(peak))
    summary("峰值增量(KB)", KB(peak - base))

    for i = 1, N, 2 do keep[i] = nil end
    collectgarbage("collect")
    local half = collectgarbage("count")
    summary("释放一半后(KB)", KB(half))
    summary("释放一半增量(KB)", KB(half - base))

    keep = nil
    collectgarbage("collect")
    local fin = collectgarbage("count")
    summary("全部释放后(KB)", KB(fin))
end

---------------------------------------------------------------------
-- 测试 5: 字符串分配压力下的 GC 吞吐量
---------------------------------------------------------------------
local function bench_strings()
    section("5. 字符串 GC 吞吐量 (字符串/秒)")
    collectgarbage("collect")
    collectgarbage("stop")

    local sum = 0
    for r = 1, ROUNDS do
        for i = 1, STRIDE_N do
            local _ = tostring(i) .. "_" .. tostring(i * i)
        end
        local t0 = os.clock()
        collectgarbage("collect")
        local t1 = os.clock()
        local dt = t1 - t0
        local thru = STRIDE_N / dt
        sum = sum + thru
        print(string.format("  round %d: GC %.4fs, 吞吐 %.0f str/s", r, dt, thru))
    end
    summary("字符串吞吐(str/s)", string.format("%.0f", sum / ROUNDS))
    collectgarbage("restart")
end

---------------------------------------------------------------------
-- 测试 6: 闭包分配压力下的 GC 吞吐量
---------------------------------------------------------------------
local function bench_closures()
    section("6. 闭包 GC 吞吐量 (闭包/秒)")
    collectgarbage("collect")
    collectgarbage("stop")

    local function make_closure(upv)
        return function() return upv + 1 end
    end

    local sum = 0
    for r = 1, ROUNDS do
        for i = 1, STRIDE_N do
            local _ = make_closure(i)
        end
        local t0 = os.clock()
        collectgarbage("collect")
        local t1 = os.clock()
        local dt = t1 - t0
        local thru = STRIDE_N / dt
        sum = sum + thru
        print(string.format("  round %d: GC %.4fs, 吞吐 %.0f cls/s", r, dt, thru))
    end
    summary("闭包吞吐(cls/s)", string.format("%.0f", sum / ROUNDS))
    collectgarbage("restart")
end

---------------------------------------------------------------------
-- 测试 7: 大配置工作集 + 多波玩家登录 (自动GC暂停分布)
--   模拟: 大量配置已加载 (长期存活), 多波玩家登录产生临时对象,
--   GC 自动运行, 测量每批次分配+GC的耗时分布, 识别暂停尖峰。
---------------------------------------------------------------------
local function bench_realistic_autogc()
    section(string.format("7. 大配置集+玩家登录 (自动GC, 配置=%d)", CONFIG_COUNT))
    collectgarbage("collect")

    -- 1. 建立长期存活的配置工作集
    local configs = {}
    for i = 1, CONFIG_COUNT do
        configs[i] = make_config(i)
    end
    collectgarbage("collect")
    local cfg_mem = collectgarbage("count")
    print(string.format("  配置工作集内存: %s", KB(cfg_mem)))

    -- 2. 调小 GC 参数让 GC 频繁触发
    local old_pause   = collectgarbage("param", "pause")
    local old_stepmul = collectgarbage("param", "stepmul")
    collectgarbage("param", "pause", 105)     -- 几乎不暂停, 频繁触发GC
    collectgarbage("param", "stepmul", 200)

    -- 3. 多波玩家登录, 每波产生临时对象
    local WAVES         = 20       -- 登录波数
    local PLAYERS_PER   = 2000     -- 每波玩家数
    local BATCH         = 100      -- 每分配100个测量一次

    local batches = {}
    local max_dt = 0
    local wave_times = {}

    for wave = 1, WAVES do
        local w0 = os.clock()
        for p = 1, PLAYERS_PER, BATCH do
            local b0 = os.clock()
            local end_p = math.min(p + BATCH - 1, PLAYERS_PER)
            for q = p, end_p do
                local _ = make_player(wave, q)
            end
            local b1 = os.clock()
            local dt = b1 - b0
            batches[#batches + 1] = dt
            if dt > max_dt then max_dt = dt end
        end
        local w1 = os.clock()
        wave_times[#wave_times + 1] = w1 - w0
        if wave % 5 == 0 then
            print(string.format("  wave %d: %.4fs, mem=%s",
                wave, w1 - w0, KB(collectgarbage("count"))))
        end
    end

    -- 4. 统计暂停分布
    table.sort(batches)
    local total_batches = #batches
    local sum_dt = 0
    for _, v in ipairs(batches) do sum_dt = sum_dt + v end

    summary("配置对象数", CONFIG_COUNT)
    summary("玩家总临时对象", WAVES * PLAYERS_PER)
    summary("测量批次数", total_batches)
    summary("自动GC最大批次(s)", SEC(max_dt))
    summary("自动GC平均批次(s)", SEC(sum_dt / total_batches))
    summary("自动GCP95批次(s)", SEC(percentile(batches, 0.95)))
    summary("自动GCP99批次(s)", SEC(percentile(batches, 0.99)))

    -- 波级统计
    table.sort(wave_times)
    local wsum = 0
    for _, v in ipairs(wave_times) do wsum = wsum + v end
    summary("最大波耗时(s)", SEC(wave_times[#wave_times]))
    summary("平均波耗时(s)", SEC(wsum / #wave_times))

    -- 5. 恢复 GC 参数并清理
    collectgarbage("param", "pause", old_pause)
    collectgarbage("param", "stepmul", old_stepmul)
    configs = nil
    collectgarbage("collect")
end

---------------------------------------------------------------------
-- 测试 8: 大配置工作集 + 手动增量step暂停分布
--   与测试7相同的工作集, 但 stop GC 后用小stepsize手动驱动,
--   精确测量每次 GC step 的暂停时间。
--   能体现 Rust (伪增量, 一步full GC) vs C (真增量) 的差异。
---------------------------------------------------------------------
local function bench_realistic_step()
    section(string.format("8. 大配置集+增量step暂停 (配置=%d, stepsize=%d)",
        CONFIG_COUNT, STEP_SIZE))
    collectgarbage("collect")
    collectgarbage("stop")

    -- 1. 建立配置工作集
    local configs = {}
    for i = 1, CONFIG_COUNT do
        configs[i] = make_config(i)
    end

    -- 2. 产生一批临时对象 (模拟一波玩家登录)
    for p = 1, 2000 do
        local _ = make_player(1, p)
    end

    -- 3. 手动 step 驱动 GC, 逐步回收, 记录每步耗时
    local steps = {}
    local max_s = 0
    local done = false
    local cnt = 0
    while not done and cnt < 50000 do
        local t0 = os.clock()
        done = collectgarbage("step", STEP_SIZE)
        local t1 = os.clock()
        local dt = t1 - t0
        steps[#steps + 1] = dt
        if dt > max_s then max_s = dt end
        cnt = cnt + 1
    end

    -- 4. 统计
    table.sort(steps)
    local sum_s = 0
    for _, v in ipairs(steps) do sum_s = sum_s + v end

    summary("配置对象数", CONFIG_COUNT)
    summary("增量步数", cnt)
    summary("大配置集最大单步(s)", SEC(max_s))
    summary("大配置集平均单步(s)", cnt > 0 and SEC(sum_s / cnt) or "N/A")
    summary("大配置集P95单步(s)", SEC(percentile(steps, 0.95)))
    summary("大配置集P99单步(s)", SEC(percentile(steps, 0.99)))

    collectgarbage("restart")
    configs = nil
    collectgarbage("collect")
end

---------------------------------------------------------------------
-- 主程序
---------------------------------------------------------------------
print("=== Lua GC 性能测试 ===")
print(string.format("Lua 版本: %s", _VERSION))
print(string.format("参数: N=%d, ROUNDS=%d, STRIDE_N=%d, STEP_SIZE=%d, CONFIG_COUNT=%d",
    N, ROUNDS, STRIDE_N, STEP_SIZE, CONFIG_COUNT))

bench_throughput()
bench_full_gc_pause()
bench_step_pause()
bench_memory()
bench_strings()
bench_closures()
bench_realistic_autogc()
bench_realistic_step()

print("\n=== 测试完成 ===")
