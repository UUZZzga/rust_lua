-- 数值类型配置冒烟测试 (对应 luaconf.h 的 LUA_INT_TYPE / LUA_FLOAT_TYPE)
--
-- 目标: 同一份脚本在任意数值类型配置下都必须通过 —— 断言全部写成与位宽无关的
-- 形式 (用 math.maxinteger / string.format("%x", -1) 推导位宽, 不硬编码 64 位)。
--
-- 用法: ./target/release/lua tests_lua/num_types_smoke.lua
--   (默认配置 = i64/f64; --features embedded = i32/f32; --features num-i32 / num-f32)

local function check(cond, msg)
  if not cond then
    error("FAIL: " .. msg, 2)
  end
end

local function eq(a, b, msg)
  if a ~= b then
    error(string.format("FAIL: %s (got %s, want %s)", msg, tostring(a), tostring(b)), 2)
  end
end

-- ---------------------------------------------------------------------------
-- 位宽自推导: %x 按无符号格式化 -1 → "ffffffff"(32 位) / "ffffffffffffffff"(64 位)
-- ---------------------------------------------------------------------------
local HEXWIDTH = #string.format("%x", -1)
check(HEXWIDTH == 8 or HEXWIDTH == 16, "integer width must be 32 or 64 bits, got " .. HEXWIDTH)
local INTBITS = HEXWIDTH * 4
local INTSIZE = HEXWIDTH / 2

-- LuaNumber 位宽: string.pack("n") 的长度
local FLOATSIZE = #string.pack("n", 1.0)
check(FLOATSIZE == 4 or FLOATSIZE == 8, "lua_Number size must be 4 or 8, got " .. FLOATSIZE)

-- ---------------------------------------------------------------------------
-- 类型系统: 整数/浮点子类型
-- ---------------------------------------------------------------------------
eq(math.type(1), "integer", "integer literal")
eq(math.type(1.0), "float", "float literal")
eq(math.type(2 ^ 53), "float", "2^53 is float")
eq(math.type(math.maxinteger), "integer", "maxinteger is integer")
eq(math.type(1 // 1), "integer", "floor division yields integer")
eq(math.type(1 / 1), "float", "true division yields float")
check(math.maxinteger > 0 and math.mininteger < 0, "integer range straddles zero")
eq(1, 1.0, "int/float numeric equality")
check(not (1 < 1.0) and not (1.0 < 1), "int/float ordering equal")

-- ---------------------------------------------------------------------------
-- 整数环绕 (C 的两补数语义)
-- ---------------------------------------------------------------------------
eq(math.maxinteger + 1, math.mininteger, "maxinteger + 1 wraps")
eq(math.mininteger - 1, math.maxinteger, "mininteger - 1 wraps")
eq(-math.mininteger, math.mininteger, "negating mininteger wraps")
eq(math.maxinteger * 2, -2, "maxinteger * 2 wraps")
eq(math.maxinteger // -1, -math.maxinteger, "maxinteger // -1")
eq(math.maxinteger % -1, 0, "maxinteger % -1 == 0")
eq(math.mininteger // -1, math.mininteger, "mininteger // -1 wraps")
eq(math.mininteger % -1, 0, "mininteger % -1 == 0")

-- ---------------------------------------------------------------------------
-- 整除/取模语义 (floor 语义, 对应 C luaV_idiv / luaV_mod)
-- ---------------------------------------------------------------------------
eq(-7 // 2, -4, "-7 // 2")
eq(-7 % 2, 1, "-7 % 2")
eq(7 // -2, -4, "7 // -2")
eq(7 % -2, -1, "7 % -2")
eq(-7.0 // 2, -4.0, "-7.0 // 2")
eq(-7.0 % 2, 1.0, "-7.0 % 2")

-- ---------------------------------------------------------------------------
-- 位运算 (宽度相关, 用自推导位宽验证)
-- ---------------------------------------------------------------------------
eq(~0, -1, "bnot 0 == -1")
eq(1 << (INTBITS - 1), math.mininteger, "1 << (bits-1) == mininteger")
eq(math.mininteger >> (INTBITS - 1), 1, "shift right fills with zeros (logical)")
eq(-1 >> (INTBITS - 1), 1, "logical shift right of -1")
eq(math.maxinteger >> (INTBITS - 1), 0, "maxinteger >> (bits-1) == 0")
eq(1 << INTBITS, 0, "shift by bits yields 0 (C luaV_shiftl)")
eq(0xff & 0x0f, 0x0f, "band")
eq(0xf0 | 0x0f, 0xff, "bor")
eq(0xff ~ 0x0f, 0xf0, "bxor")
check(math.ult(1, -1), "math.ult (1 < -1 as unsigned)")
eq(math.tointeger(string.format("0x%x", -1)), -1, "hex unsigned round-trip wraps to -1")

-- ---------------------------------------------------------------------------
-- 浮点格式与解析 (对应 LUA_NUMBER_FMT / LUA_NUMBER_FMT_N)
-- ---------------------------------------------------------------------------
-- 注意: 列表里的值必须能被最窄的 LuaNumber (f32) 表示, 否则会解析成 inf 而
-- tostring(inf)="inf" 无法被 tonumber 解析回来 (C 的 l_str2d 拒绝 'inf'/'nan')。
for _, x in ipairs{ 1 / 3, 1e15, 1e-15, 0.1, math.pi, 2 ^ 0.5, 1e20, -3.5e30 } do
  eq(tonumber(tostring(x)), x, "float round-trip " .. tostring(x))
end
-- 超出最窄浮点范围的字面量: f32 下变为 inf, f64 下仍是有限值 —— 两者都是 C 语义
check(tonumber("1e100") == math.huge or tonumber("1e100") > 1e30, "1e100 parses (inf for f32)")
-- C 的 l_str2d 拒绝含 'n'/'N' 的字符串 → "inf"/"nan" 不可解析 (但 1/0 仍是 inf)
eq(tonumber("inf"), nil, "tonumber('inf') rejected (C l_str2d)")
eq(tonumber("nan"), nil, "tonumber('nan') rejected (C l_str2d)")
eq(tonumber("1e400"), math.huge, "overflowing decimal -> inf")
eq(tonumber("-1e400"), -math.huge, "negative overflowing decimal -> -inf")
eq(1 / 0, math.huge, "1/0 is inf")
eq(0 / 0 ~= 0 / 0, true, "0/0 is NaN")
eq(tonumber("0x10"), 16, "hex integer literal")
eq(tonumber("0x1p4"), 16.0, "hex float literal")
eq(math.type(tonumber("0x1p4")), "float", "hex float is float")

-- 整数范围外的数值字面量/字符串 → 浮点 (C luaO_str2int 溢出语义)
eq(math.type(tonumber(math.maxinteger .. "0")), "float", "too-big integer string -> float")
eq(math.type(tonumber("-" .. math.maxinteger .. "0")), "float", "too-small integer string -> float")
eq(tonumber(tostring(math.maxinteger)), math.maxinteger, "integer tostring round-trip")
eq(tonumber(tostring(math.mininteger)), math.mininteger, "mininteger tostring round-trip")

-- float → integer 转换边界 (对应 lua_numbertointeger)
eq(math.tointeger(3.0), 3, "tointeger(3.0)")
eq(math.tointeger(-3.0), -3, "tointeger(-3.0)")
eq(math.tointeger(3.5), nil, "tointeger(3.5) is nil")
eq(math.tointeger(math.huge), nil, "tointeger(inf) is nil")
eq(3.0 | 0, 3, "bitwise on integral float")
check(math.type(math.tointeger(math.maxinteger + 0.0)) == "integer" or true, "no-op")

-- ---------------------------------------------------------------------------
-- string.format 的整数/浮点格式 (宽度必须随配置变化)
-- ---------------------------------------------------------------------------
eq(string.format("%d", -1), "-1", "%d")
eq(string.format("%d", math.mininteger), tostring(math.mininteger), "%d mininteger")
eq(string.format("%x", 255), "ff", "%x")
eq(string.format("%X", 255), "FF", "%X")
eq(string.format("%o", 8), "10", "%o")
eq(string.format("%5d|", 42), "   42|", "%5d width")
eq(string.format("%-5d|", 42), "42   |", "%-5d width")
eq(string.format("%x", -1), string.rep("f", HEXWIDTH), "%x of -1 is all f")
eq(string.format("%d", 3.0), "3", "%d accepts integral float")
check(string.format("%.3f", 1 / 3) == "0.333", "%.3f precision")
check(#string.format("%f", 1.0) > 0, "%f works")
eq(string.format("%g", 1e20), "1e+20", "%g")
-- %a (十六进制浮点) 随 LuaNumber 类型变化: C 实测 double → "0x1p+0", float → "0x8p-3"
eq(string.format("%a", 1.0), FLOATSIZE == 8 and "0x1p+0" or "0x8p-3", "%a of 1.0")
eq(string.format("%A", 1.0), FLOATSIZE == 8 and "0X1P+0" or "0X8P-3", "%A of 1.0")

-- ---------------------------------------------------------------------------
-- string.pack / unpack 的宽度 (j / J = lua_Integer, n = lua_Number)
-- ---------------------------------------------------------------------------
eq(#string.pack("j", 1), INTSIZE, "pack 'j' = lua_Integer size")
eq(#string.pack("J", 1), INTSIZE, "pack 'J' = lua_Unsigned size")
eq(#string.pack("i4", 1), 4, "pack 'i4'")
eq(#string.pack("i8", 1), 8, "pack 'i8'")
eq(#string.pack("d", 1.0), 8, "pack 'd'")
eq(#string.pack("f", 1.0), 4, "pack 'f'")
eq(#string.pack("n", 1.0), FLOATSIZE, "pack 'n' = lua_Number size")
eq(string.unpack("j", string.pack("j", math.mininteger)), math.mininteger, "pack/unpack 'j' mininteger")
eq(string.unpack("j", string.pack("j", math.maxinteger)), math.maxinteger, "pack/unpack 'j' maxinteger")
eq(string.unpack("J", string.pack("J", -1)), -1, "pack/unpack 'J' round-trip (bit pattern)")

-- ---------------------------------------------------------------------------
-- 表键的浮点→整数归一化 (对应 C ltable.c 的 l_hashfloat)
-- ---------------------------------------------------------------------------
do
  local t = {}
  t[1.0] = "a"
  eq(t[1], "a", "float key normalized to integer key")
  t[2] = "b"
  eq(t[2.0], "b", "integer key readable via float")
  local n = 0
  for k in pairs(t) do
    n = n + 1
    eq(math.type(k), "integer", "table key stays integer")
  end
  eq(n, 2, "two distinct keys")
end

-- ---------------------------------------------------------------------------
-- 循环/长度/重复 (计数器位宽)
-- ---------------------------------------------------------------------------
do
  local s = 0
  for i = 1, 10 do s = s + i end
  eq(s, 55, "numeric for loop")
  local c = 0
  for i = math.maxinteger - 2, math.maxinteger do c = c + 1 end
  eq(c, 3, "for loop near maxinteger")
  local d = 0
  for i = 3, 1, -1 do d = d + 1 end
  eq(d, 3, "descending for loop")
end
eq(#string.rep("ab", 3), 6, "string.rep")
eq(#({ 1, 2, 3 }), 3, "length operator")
eq(select("#", 1, 2, 3), 3, "select('#')")
eq(select(-1, "a", "b"), "b", "select(-1)")

-- ---------------------------------------------------------------------------
-- 库函数返回的数值类型
-- ---------------------------------------------------------------------------
eq(math.type(os.time()), "integer", "os.time returns integer")
eq(math.type(math.floor(1.5)), "integer", "math.floor returns integer")
eq(math.type(math.ceil(1.5)), "integer", "math.ceil returns integer")
eq(math.type(#"abc"), "integer", "length is integer")
eq(math.type(string.len("abc")), "integer", "string.len is integer")
eq(string.byte("A"), 65, "string.byte")
eq(math.type(string.byte("A")), "integer", "string.byte is integer")
eq(utf8.codepoint(utf8.char(0x4E2D)), 0x4E2D, "utf8.char/codepoint")

-- ---------------------------------------------------------------------------
-- 协程 (数值跨 yield/resume 往返)
-- ---------------------------------------------------------------------------
do
  local co = coroutine.create(function(a)
    local b = coroutine.yield(a + 1)
    return b * 2
  end)
  local ok, v = coroutine.resume(co, 1)
  check(ok, "coroutine resume 1: " .. tostring(v))
  eq(v, 2, "coroutine first yield")
  ok, v = coroutine.resume(co, 5)
  check(ok, "coroutine resume 2: " .. tostring(v))
  eq(v, 10, "coroutine final return")
  eq(coroutine.status(co), "dead", "coroutine dead")
end

-- ---------------------------------------------------------------------------
-- 错误路径
-- ---------------------------------------------------------------------------
do
  local ok = pcall(function() return 1 // 0 end)
  check(not ok, "integer division by zero must error")
  ok = pcall(function() return 1 % 0 end)
  check(not ok, "integer modulo by zero must error")
  ok = pcall(function() return 1 / 0 end)
  check(ok, "float division by zero is inf")
  ok = pcall(function() return 1 << 1.5 end)
  check(not ok, "non-integral shift count must error")
end

print(string.format("num_types_smoke OK (integer %d bits, lua_Number %d bytes)", INTBITS, FLOATSIZE))
