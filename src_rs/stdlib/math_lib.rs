//! 数学库 (lmathlib.cpp → Rust)
//!
//! 对应 C 源码: lmathlib.cpp
//!
//! ## 主要功能
//! - 注册数学库全局表 `math`，包含所有数学函数和常量
//! - 提供基本数学函数: abs, floor, ceil, sqrt, exp, log, sin, cos, tan 等
//! - 提供三角函数和反三角函数: asin, acos, atan, deg, rad
//! - 提供数值操作: fmod, modf, frexp, ldexp, tointeger, ult
//! - 提供 min/max 函数
//! - 提供 math.type 函数 (区分 integer/float)
//! - 提供伪随机数生成器 (xoshiro256** 算法): random, randomseed
//! - 提供常量: pi, huge, maxinteger, mininteger

use crate::objects::{NilKind, TValue};
use crate::state::LuaState;
use crate::table::Table;
use crate::execute::VmError;

// ============================================================================
// 常量
// ============================================================================

/// 圆周率 PI — 对应 C 的 PI 宏
pub const PI: f64 = 3.141592653589793238462643383279502884;

/// 浮点数最大值 — 对应 C 的 HUGE_VAL
pub const HUGE: f64 = f64::INFINITY;

/// 最大整数 — 对应 C 的 LUA_MAXINTEGER
pub const MAX_INTEGER: i64 = i64::MAX;

/// 最小整数 — 对应 C 的 LUA_MININTEGER
pub const MIN_INTEGER: i64 = i64::MIN;

// ============================================================================
// 函数标签 (LightUserData 占位符值)
// ============================================================================
// 标签 1-19: 基础库
// 标签 100+: 字符串库
// 标签 200+: 数学库

pub const MATH_ABS: usize = 200;
pub const MATH_ACOS: usize = 201;
pub const MATH_ASIN: usize = 202;
pub const MATH_ATAN: usize = 203;
pub const MATH_CEIL: usize = 204;
pub const MATH_COS: usize = 205;
pub const MATH_DEG: usize = 206;
pub const MATH_EXP: usize = 207;
pub const MATH_TOINTEGER: usize = 208;
pub const MATH_FLOOR: usize = 209;
pub const MATH_FMOD: usize = 210;
pub const MATH_FREXP: usize = 211;
pub const MATH_ULT: usize = 212;
pub const MATH_LDEXP: usize = 213;
pub const MATH_LOG: usize = 214;
pub const MATH_MAX: usize = 215;
pub const MATH_MIN: usize = 216;
pub const MATH_MODF: usize = 217;
pub const MATH_RAD: usize = 218;
pub const MATH_SIN: usize = 219;
pub const MATH_SQRT: usize = 220;
pub const MATH_TAN: usize = 221;
pub const MATH_TYPE: usize = 222;
pub const MATH_RANDOM: usize = 223;
pub const MATH_RANDOMSEED: usize = 224;

/// 数学库标签范围: [200, 300)
pub fn is_math_tag(tag: usize) -> bool {
    (200..300).contains(&tag)
}

/// 将 math 库函数 tag 映射到函数名（用于 traceback）
pub fn math_function_name(tag: usize) -> Option<&'static str> {
    match tag {
        MATH_ABS => Some("abs"),
        MATH_ACOS => Some("acos"),
        MATH_ASIN => Some("asin"),
        MATH_ATAN => Some("atan"),
        MATH_CEIL => Some("ceil"),
        MATH_COS => Some("cos"),
        MATH_DEG => Some("deg"),
        MATH_EXP => Some("exp"),
        MATH_TOINTEGER => Some("tointeger"),
        MATH_FLOOR => Some("floor"),
        MATH_FMOD => Some("fmod"),
        MATH_FREXP => Some("frexp"),
        MATH_ULT => Some("ult"),
        MATH_LDEXP => Some("ldexp"),
        MATH_LOG => Some("log"),
        MATH_MAX => Some("max"),
        MATH_MIN => Some("min"),
        MATH_MODF => Some("modf"),
        MATH_RAD => Some("rad"),
        MATH_SIN => Some("sin"),
        MATH_SQRT => Some("sqrt"),
        MATH_TAN => Some("tan"),
        MATH_TYPE => Some("type"),
        MATH_RANDOM => Some("random"),
        MATH_RANDOMSEED => Some("randomseed"),
        _ => None,
    }
}

// ============================================================================
// 纯函数实现 (无状态, 可独立测试)
// ============================================================================

/// math.abs(v) — 绝对值 (对应 C 的 math_abs)
///
/// 整数输入返回整数, 浮点输入返回浮点。
/// 整数最小值取绝对值会产生回绕 (与 C 行为一致)。
pub fn math_abs(v: &TValue) -> Result<TValue, String> {
    match v {
        TValue::Integer(n) => {
            // 对应 C: if (n < 0) n = (lua_Integer)(0u - (lua_Unsigned)n);
            Ok(TValue::Integer(n.wrapping_abs()))
        }
        TValue::Float(f) => Ok(TValue::Float(f.abs())),
        _ => Err(format!("bad argument #1 to 'abs' (number expected, got {})", v.ty())),
    }
}

/// math.sin(x) — 正弦函数 (对应 C 的 math_sin)
pub fn math_sin(x: f64) -> f64 {
    x.sin()
}

/// math.cos(x) — 余弦函数 (对应 C 的 math_cos)
pub fn math_cos(x: f64) -> f64 {
    x.cos()
}

/// math.tan(x) — 正切函数 (对应 C 的 math_tan)
pub fn math_tan(x: f64) -> f64 {
    x.tan()
}

/// math.asin(x) — 反正弦函数 (对应 C 的 math_asin)
pub fn math_asin(x: f64) -> f64 {
    x.asin()
}

/// math.acos(x) — 反余弦函数 (对应 C 的 math_acos)
pub fn math_acos(x: f64) -> f64 {
    x.acos()
}

/// math.atan(y [, x]) — 反正切函数 (对应 C 的 math_atan)
///
/// 单参数: atan(y)
/// 双参数: atan2(y, x)
pub fn math_atan(y: f64, x: Option<f64>) -> f64 {
    match x {
        None => y.atan(),
        Some(x) => y.atan2(x),
    }
}

/// math.tointeger(v) — 转换为整数 (对应 C 的 math_toint)
///
/// 返回 Some(整数) 表示成功, None 表示不可转换。
pub fn math_tointeger(v: &TValue) -> Option<i64> {
    match v {
        TValue::Integer(n) => Some(*n),
        TValue::Float(f) => crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq),
        TValue::Str(s) => {
            // 对应 C 的 luaV_tointeger: l_strton 转成数字，再转整数
            match crate::objects::str2num(s.as_str()) {
                Some(TValue::Integer(n)) => Some(n),
                Some(TValue::Float(f)) => crate::vm::float_to_integer(f, crate::vm::F2IMode::Eq),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 将浮点数转为整数 (如果可以无损转换), 否则保持浮点
/// 对应 C 的 pushnumint
fn push_num_int(d: f64) -> TValue {
    // C 的 lua_numbertointeger: d >= MININTEGER && d < -MININTEGER
    // -MININTEGER = 2^63 (可精确表示为 f64)
    let min_int_f = i64::MIN as f64;
    let max_int_f = -min_int_f;
    if d >= min_int_f && d < max_int_f {
        TValue::Integer(d as i64)
    } else {
        TValue::Float(d)
    }
}

/// math.floor(v) — 向下取整 (对应 C 的 math_floor)
///
/// 整数输入返回自身, 浮点输入返回 floor (可能为整数或浮点)。
pub fn math_floor(v: &TValue) -> Result<TValue, String> {
    match v {
        TValue::Integer(_) => Ok(v.clone()),
        TValue::Float(f) => Ok(push_num_int(f.floor())),
        _ => Err(format!("bad argument #1 to 'floor' (number expected, got {})", v.ty())),
    }
}

/// math.ceil(v) — 向上取整 (对应 C 的 math_ceil)
///
/// 整数输入返回自身, 浮点输入返回 ceil (可能为整数或浮点)。
pub fn math_ceil(v: &TValue) -> Result<TValue, String> {
    match v {
        TValue::Integer(_) => Ok(v.clone()),
        TValue::Float(f) => Ok(push_num_int(f.ceil())),
        _ => Err(format!("bad argument #1 to 'ceil' (number expected, got {})", v.ty())),
    }
}

/// math.fmod(v1, v2) — 取模 (对应 C 的 math_fmod)
///
/// 整数操作数: 整数取模 (C 的 % 运算符)
/// 浮点操作数: 浮点取模 (C 的 fmod)
pub fn math_fmod(v1: &TValue, v2: &TValue) -> Result<TValue, String> {
    match (v1, v2) {
        (TValue::Integer(a), TValue::Integer(b)) => {
            if *b == 0 {
                return Err("bad argument #2 to 'fmod' (zero)".to_string());
            }
            if *b == -1 {
                // 避免溢出: 任何数 / -1 的余数都是 0
                return Ok(TValue::Integer(0));
            }
            Ok(TValue::Integer(a.wrapping_rem(*b)))
        }
        _ => {
            let a = to_float(v1)?;
            let b = to_float(v2).map_err(|_| {
                format!("bad argument #2 to 'fmod' (number expected, got {})", v2.ty())
            })?;
            Ok(TValue::Float(a % b))
        }
    }
}

/// math.modf(v) — 分离整数和小数部分 (对应 C 的 math_modf)
///
/// 返回 (整数部分, 小数部分)。
/// 整数输入: (自身, 0.0)
/// 浮点输入: (向零取整的整数部分, 小数部分)
pub fn math_modf(v: &TValue) -> Result<(TValue, TValue), String> {
    match v {
        TValue::Integer(_) => {
            Ok((v.clone(), TValue::Float(0.0)))
        }
        TValue::Float(f) => {
            // 对应 C: ip = (n < 0) ? ceil(n) : floor(n)
            // NaN/Inf 走正常路径: NaN 的 frac 为 NaN, Inf 的 frac 为 0.0
            let ip = if *f < 0.0 { f.ceil() } else { f.floor() };
            let frac = if *f == ip { 0.0 } else { *f - ip };
            Ok((push_num_int(ip), TValue::Float(frac)))
        }
        _ => Err(format!("bad argument #1 to 'modf' (number expected, got {})", v.ty())),
    }
}

/// math.sqrt(x) — 平方根 (对应 C 的 math_sqrt)
pub fn math_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// math.ult(a, b) — 无符号小于比较 (对应 C 的 math_ult)
///
/// 将两个整数转为无符号后比较 a < b。
pub fn math_ult(a: i64, b: i64) -> bool {
    (a as u64) < (b as u64)
}

/// math.log(x [, base]) — 对数 (对应 C 的 math_log)
///
/// 无 base: 自然对数 ln(x)
/// base == 2: log2(x)
/// base == 10: log10(x)
/// 其他: ln(x) / ln(base)
pub fn math_log(x: f64, base: Option<f64>) -> f64 {
    match base {
        None => x.ln(),
        Some(b) => {
            if b == 2.0 {
                x.log2()
            } else if b == 10.0 {
                x.log10()
            } else {
                x.ln() / b.ln()
            }
        }
    }
}

/// math.exp(x) — 指数函数 e^x (对应 C 的 math_exp)
pub fn math_exp(x: f64) -> f64 {
    x.exp()
}

/// math.deg(x) — 弧度转角度 (对应 C 的 math_deg)
pub fn math_deg(x: f64) -> f64 {
    x * (180.0 / PI)
}

/// math.rad(x) — 角度转弧度 (对应 C 的 math_rad)
pub fn math_rad(x: f64) -> f64 {
    x * (PI / 180.0)
}

/// math.frexp(x) — 分离尾数和指数 (对应 C 的 math_frexp)
///
/// 返回 (尾数 m, 指数 e), 使得 x = m * 2^e, 其中 0.5 <= |m| < 1 或 m == 0。
pub fn math_frexp(x: f64) -> (f64, i64) {
    if x == 0.0 {
        return (0.0, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1022;
    let mantissa_bits = (bits & 0x800f_ffff_ffff_ffff) | 0x3fe0_0000_0000_0000;
    let m = f64::from_bits(mantissa_bits);
    (m, exp)
}

/// math.ldexp(x, e) — x * 2^e (对应 C 的 math_ldexp)
pub fn math_ldexp(x: f64, e: i64) -> f64 {
    x * (2.0_f64).powf(e as f64)
}

/// math.min(...) — 最小值 (对应 C 的 math_min)
///
/// 比较所有参数, 返回最小值。
/// 混合整数和浮点时, 比较使用 Lua 的 < 运算符语义。
pub fn math_min(args: &[TValue]) -> Result<TValue, String> {
    if args.is_empty() {
        return Err("bad argument #1 to 'min' (value expected)".to_string());
    }
    let mut min_idx = 0;
    for i in 1..args.len() {
        if lua_lt(&args[i], &args[min_idx])? {
            min_idx = i;
        }
    }
    Ok(args[min_idx].clone())
}

/// math.max(...) — 最大值 (对应 C 的 math_max)
pub fn math_max(args: &[TValue]) -> Result<TValue, String> {
    if args.is_empty() {
        return Err("bad argument #1 to 'max' (value expected)".to_string());
    }
    let mut max_idx = 0;
    for i in 1..args.len() {
        if lua_lt(&args[max_idx], &args[i])? {
            max_idx = i;
        }
    }
    Ok(args[max_idx].clone())
}

/// math.type(v) — 返回数字子类型 (对应 C 的 math_type)
///
/// 整数返回 "integer", 浮点返回 "float", 其他返回 None。
pub fn math_type(v: &TValue) -> Option<&'static str> {
    match v {
        TValue::Integer(_) => Some("integer"),
        TValue::Float(_) => Some("float"),
        _ => None,
    }
}

// ============================================================================
// 辅助函数: TValue 与数字的转换
// ============================================================================

/// 将 TValue 转为浮点数 (对应 C 的 luaL_checknumber)
fn to_float(v: &TValue) -> Result<f64, String> {
    match v {
        TValue::Integer(n) => Ok(*n as f64),
        TValue::Float(f) => Ok(*f),
        TValue::Str(s) => {
            let s = s.as_str();
            s.parse::<f64>().map_err(|_| {
                format!("bad argument (number expected, got string '{}')", s)
            })
        }
        _ => Err(format!("bad argument (number expected, got {})", v.ty())),
    }
}

/// 将 TValue 转为整数 (对应 C 的 luaL_checkinteger)
fn to_integer(v: &TValue) -> Result<i64, String> {
    match v {
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => {
            let i = *f as i64;
            if (i as f64) == *f {
                Ok(i)
            } else {
                Err(format!("bad argument (integer expected, got float {})", f))
            }
        }
        _ => Err(format!("bad argument (integer expected, got {})", v.ty())),
    }
}

/// Lua 的 < 比较运算 (对应 C 的 lua_compare LUA_OPLT)
///
/// 数值比较: 整数和浮点混合时按数值比较。
/// 字符串比较: 字典序。
fn lua_lt(a: &TValue, b: &TValue) -> Result<bool, String> {
    match (a, b) {
        (TValue::Integer(x), TValue::Integer(y)) => Ok(x < y),
        (TValue::Float(x), TValue::Float(y)) => Ok(x < y),
        (TValue::Integer(x), TValue::Float(y)) => Ok((*x as f64) < *y),
        (TValue::Float(x), TValue::Integer(y)) => Ok(*x < (*y as f64)),
        (TValue::Str(x), TValue::Str(y)) => Ok(x.as_str() < y.as_str()),
        _ => Err(format!(
            "attempt to compare {} with {}",
            a.ty(), b.ty()
        )),
    }
}

// ============================================================================
// 伪随机数生成器 (xoshiro256** 算法)
// 对应 C 源码 lmathlib.cpp 的 PRN generator 部分
// ============================================================================

/// 随机数生成器状态 — 对应 C 的 RanState
#[derive(Debug, Clone)]
pub struct RandState {
    /// 4 个 64 位状态字 (对应 C 的 Rand64 s[4])
    pub s: [u64; 4],
}

impl RandState {
    /// 创建新的随机状态 (全零, 需要设置种子)
    pub fn new() -> Self {
        RandState { s: [0, 0, 0, 0] }
    }

    /// 64 位左旋转
    /// 对应 C 的 rotl
    fn rotl(x: u64, n: u32) -> u64 {
        (x << n) | (x >> (64 - n))
    }

    /// 生成下一个随机数 — 对应 C 的 nextrand
    ///
    /// xoshiro256** 算法核心
    pub fn nextrand(&mut self) -> u64 {
        let state0 = self.s[0];
        let state1 = self.s[1];
        let state2 = self.s[2] ^ state0;
        let state3 = self.s[3] ^ state1;
        let res = Self::rotl(state1.wrapping_mul(5), 7).wrapping_mul(9);
        self.s[0] = state0 ^ state3;
        self.s[1] = state1 ^ state2;
        self.s[2] = state2 ^ (state1 << 17);
        self.s[3] = Self::rotl(state3, 45);
        res
    }

    /// 将随机整数转为 [0, 1) 范围的浮点数 — 对应 C 的 I2d
    ///
    /// 取高 53 位 (f64 的尾数位数), 转为浮点数。
    fn i2d(x: u64) -> f64 {
        // 对应 C: 取高 FIGS 位 (53 for double), 乘以 2^(-53)
        // shift64_FIG = 64 - 53 = 11
        // scaleFIG = 0.5 / (1 << 52) = 2^(-53)
        let sx = (x >> 11) as i64;
        let mut res = (sx as f64) * (1.0 / (1u64 << 53) as f64);
        if sx < 0 {
            // 修正负数的二进制补码
            res += 1.0;
        }
        res
    }

    /// 将随机整数投影到 [0, n] 区间 — 对应 C 的 project
    ///
    /// 使用拒绝采样确保均匀分布。
    fn project(&mut self, mut ran: u64, n: u64) -> u64 {
        if n == u64::MAX {
            return ran;
        }
        // 计算 >= n 的最小 Mersenne 数 (2^k - 1)
        let mut lim = n;
        let mut sh = 1u32;
        while (lim & (lim.wrapping_add(1))) != 0 {
            lim |= lim >> sh;
            sh *= 2;
        }
        // 拒绝采样
        loop {
            ran &= lim;
            if ran <= n {
                return ran;
            }
            ran = self.nextrand();
        }
    }

    /// 设置种子 — 对应 C 的 setseed
    ///
    /// 使用两个 64 位种子初始化状态, 然后丢弃前 16 个随机数以扩散种子。
    pub fn setseed(&mut self, n1: u64, n2: u64) {
        self.s[0] = n1;
        self.s[1] = 0xff; // 避免全零状态
        self.s[2] = n2;
        self.s[3] = 0;
        // 丢弃前 16 个值以扩散种子
        for _ in 0..16 {
            self.nextrand();
        }
    }
}

impl Default for RandState {
    fn default() -> Self {
        let mut state = RandState::new();
        // 使用固定默认种子 (对应 C 中 luaL_makeseed 失败时的回退)
        state.setseed(1, 0);
        state
    }
}

/// math.random([low [, up]]) — 伪随机数 (对应 C 的 math_random)
///
/// 无参数: 返回 [0, 1) 范围的浮点数
/// 单参数 0: 返回全范围随机整数
/// 单参数 n: 返回 [1, n] 范围的整数
/// 双参数 low, up: 返回 [low, up] 范围的整数
pub fn math_random(
    state: &mut RandState,
    args: &[TValue],
) -> Result<TValue, String> {
    let rv = state.nextrand();
    match args.len() {
        0 => {
            // 无参数: 返回 [0, 1) 浮点数
            Ok(TValue::Float(RandState::i2d(rv)))
        }
        1 => {
            let up = to_integer(&args[0])?;
            if up == 0 {
                // 单个 0: 返回全范围随机整数
                Ok(TValue::Integer(rv as i64))
            } else {
                let low = 1i64;
                if low > up {
                    return Err("bad argument #1 to 'random' (interval is empty)".to_string());
                }
                // 投影到 [0, up - low] 然后加 low
                let p = state.project(rv, (up as u64).wrapping_sub(low as u64));
                Ok(TValue::Integer(p.wrapping_add(low as u64) as i64))
            }
        }
        2 => {
            let low = to_integer(&args[0])?;
            let up = to_integer(&args[1])?;
            if low > up {
                return Err("bad argument #1 to 'random' (interval is empty)".to_string());
            }
            let p = state.project(rv, (up as u64).wrapping_sub(low as u64));
            Ok(TValue::Integer(p.wrapping_add(low as u64) as i64))
        }
        _ => Err("wrong number of arguments to 'random'".to_string()),
    }
}

/// math.randomseed([x [, y]]) — 设置随机种子 (对应 C 的 math_randomseed)
///
/// 无参数: 使用时间生成种子
/// 单参数 x: 使用 x 作为主种子, 0 作为次种子
/// 双参数 x, y: 使用 x, y 作为种子
///
/// 返回 (主种子, 次种子)
pub fn math_randomseed(
    state: &mut RandState,
    args: &[TValue],
) -> Result<(i64, i64), String> {
    let (n1, n2) = match args.len() {
        0 => {
            // 使用当前时间作为种子 (对应 C 的 luaL_makeseed)
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            // 如果时间种子不够随机, 用 nextrand 补充
            let n2 = state.nextrand();
            (seed, n2)
        }
        1 => {
            let n1 = to_integer(&args[0])? as u64;
            (n1, 0)
        }
        2 => {
            let n1 = to_integer(&args[0])? as u64;
            let n2 = to_integer(&args[1])? as u64;
            (n1, n2)
        }
        _ => return Err("wrong number of arguments to 'randomseed'".to_string()),
    };
    state.setseed(n1, n2);
    Ok((n1 as i64, n2 as i64))
}

// ============================================================================
// 栈操作辅助函数
// ============================================================================

/// 从栈中读取参数 (0-based 索引, 相对于函数位置 a)
fn get_arg(state: &LuaState, a: usize, idx: usize) -> TValue {
    let stack_idx = a + 1 + idx;
    if stack_idx < state.stack.len() {
        state.stack[stack_idx].clone()
    } else {
        TValue::Nil(NilKind::Strict)
    }
}

/// 将结果压入栈并调整栈顶
fn push_results(state: &mut LuaState, a: usize, nresults: i32, results: Vec<TValue>) {
    state.adjust_results(a, nresults, results);
}

/// 将单个结果压入栈
fn push_single_result(state: &mut LuaState, a: usize, nresults: i32, result: TValue) {
    push_results(state, a, nresults, vec![result]);
}

/// 从栈中读取数字参数 (整数或浮点)
fn get_number_arg(state: &LuaState, a: usize, idx: usize, fname: &str) -> Result<TValue, VmError> {
    let v = get_arg(state, a, idx);
    match &v {
        TValue::Integer(_) | TValue::Float(_) => Ok(v),
        TValue::Str(s) => {
            // 尝试解析字符串为数字
            let s = s.as_str();
            if let Ok(i) = s.parse::<i64>() {
                Ok(TValue::Integer(i))
            } else if let Ok(f) = s.parse::<f64>() {
                Ok(TValue::Float(f))
            } else {
                Err(VmError::RuntimeError(format!(
                    "bad argument #{} to '{}' (number expected, got string '{}')",
                    idx + 1, fname, s
                )))
            }
        }
        TValue::Nil(_) => Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (number expected, got nil)",
            idx + 1, fname
        ))),
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (number expected, got {})",
            idx + 1, fname, crate::tm::obj_type_name(&v)
        ))),
    }
}

/// 从栈中读取可选数字参数, 缺失时返回默认值
fn get_opt_number_arg(state: &LuaState, a: usize, idx: usize, default: f64) -> f64 {
    let v = get_arg(state, a, idx);
    match &v {
        TValue::Integer(n) => *n as f64,
        TValue::Float(f) => *f,
        TValue::Nil(_) => default,
        _ => default,
    }
}

/// 从栈中读取整数参数
fn get_int_arg(state: &LuaState, a: usize, idx: usize, fname: &str) -> Result<i64, VmError> {
    let v = get_arg(state, a, idx);
    match &v {
        TValue::Integer(n) => Ok(*n),
        TValue::Float(f) => {
            let i = *f as i64;
            if (i as f64) == *f {
                Ok(i)
            } else {
                Err(VmError::RuntimeError(format!(
                    "bad argument #{} to '{}' (integer expected, got float {})",
                    idx + 1, fname, f
                )))
            }
        }
        _ => Err(VmError::RuntimeError(format!(
            "bad argument #{} to '{}' (integer expected, got {})",
            idx + 1, fname, v.ty()
        ))),
    }
}

// ============================================================================
// 派发函数 — 从 execute.rs 的 op_call 调用
// ============================================================================

/// 数学库函数派发
///
/// 从 execute.rs 的 op_call 或 op_tailcall 调用,
/// 当 LightUserData 标签在 [200, 300) 范围内时。
pub fn call_math_function(
    tag: usize,
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
) -> Result<(), VmError> {
    // 设置当前 C 函数名（用于 traceback）
    let prev_c_func = state.last_c_function.take();
    state.last_c_function = math_function_name(tag).map(|s| s.to_string());

    let result = match tag {
        MATH_ABS => call_abs(state, a, nargs, nresults),
        MATH_SIN => call_simple_unary(state, a, nargs, nresults, math_sin, "sin"),
        MATH_COS => call_simple_unary(state, a, nargs, nresults, math_cos, "cos"),
        MATH_TAN => call_simple_unary(state, a, nargs, nresults, math_tan, "tan"),
        MATH_ASIN => call_simple_unary(state, a, nargs, nresults, math_asin, "asin"),
        MATH_ACOS => call_simple_unary(state, a, nargs, nresults, math_acos, "acos"),
        MATH_ATAN => call_atan(state, a, nargs, nresults),
        MATH_DEG => call_simple_unary(state, a, nargs, nresults, math_deg, "deg"),
        MATH_RAD => call_simple_unary(state, a, nargs, nresults, math_rad, "rad"),
        MATH_EXP => call_simple_unary(state, a, nargs, nresults, math_exp, "exp"),
        MATH_SQRT => call_simple_unary(state, a, nargs, nresults, math_sqrt, "sqrt"),
        MATH_LOG => call_log(state, a, nargs, nresults),
        MATH_FLOOR => call_floor(state, a, nargs, nresults),
        MATH_CEIL => call_ceil(state, a, nargs, nresults),
        MATH_FMOD => call_fmod(state, a, nargs, nresults),
        MATH_MODF => call_modf(state, a, nargs, nresults),
        MATH_TOINTEGER => call_tointeger(state, a, nargs, nresults),
        MATH_ULT => call_ult(state, a, nargs, nresults),
        MATH_FREXP => call_frexp(state, a, nargs, nresults),
        MATH_LDEXP => call_ldexp(state, a, nargs, nresults),
        MATH_MIN => call_min(state, a, nargs, nresults),
        MATH_MAX => call_max(state, a, nargs, nresults),
        MATH_TYPE => call_type(state, a, nargs, nresults),
        MATH_RANDOM => call_random(state, a, nargs, nresults),
        MATH_RANDOMSEED => call_randomseed(state, a, nargs, nresults),
        _ => Err(VmError::RuntimeError(format!("unknown math function tag: {}", tag))),
    };

    if result.is_ok() {
        state.last_c_function = prev_c_func;
    }
    result
}

// ============================================================================
// 各函数的派发实现
// ============================================================================

/// math.abs(v) — 对应 C 的 math_abs
fn call_abs(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'abs' (number expected, got no value)".to_string(),
        ));
    }
    let v = get_number_arg(state, a, 0, "abs")?;
    match math_abs(&v) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// 通用一元浮点函数派发 — 用于 sin/cos/tan/asin/acos/deg/rad/exp/sqrt
fn call_simple_unary(
    state: &mut LuaState,
    a: usize,
    nargs: usize,
    nresults: i32,
    f: fn(f64) -> f64,
    fname: &str,
) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(format!(
            "bad argument #1 to '{}' (number expected, got no value)",
            fname
        )));
    }
    let v = get_number_arg(state, a, 0, fname)?;
    let x = to_float(&v).map_err(|msg| VmError::RuntimeError(msg))?;
    let result = f(x);
    push_single_result(state, a, nresults, TValue::Float(result));
    Ok(())
}

/// math.atan(y [, x]) — 对应 C 的 math_atan
fn call_atan(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'atan' (number expected, got no value)".to_string(),
        ));
    }
    let yv = get_number_arg(state, a, 0, "atan")?;
    let y = to_float(&yv).map_err(|msg| VmError::RuntimeError(msg))?;
    let x = if nargs >= 2 {
        let xv = get_number_arg(state, a, 1, "atan")?;
        Some(to_float(&xv).map_err(|msg| VmError::RuntimeError(msg))?)
    } else {
        None
    };
    let result = math_atan(y, x);
    push_single_result(state, a, nresults, TValue::Float(result));
    Ok(())
}

/// math.log(x [, base]) — 对应 C 的 math_log
fn call_log(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'log' (number expected, got no value)".to_string(),
        ));
    }
    let xv = get_number_arg(state, a, 0, "log")?;
    let x = to_float(&xv).map_err(|msg| VmError::RuntimeError(msg))?;
    let base = if nargs >= 2 {
        let bv = get_number_arg(state, a, 1, "log")?;
        Some(to_float(&bv).map_err(|msg| VmError::RuntimeError(msg))?)
    } else {
        None
    };
    let result = math_log(x, base);
    push_single_result(state, a, nresults, TValue::Float(result));
    Ok(())
}

/// math.floor(v) — 对应 C 的 math_floor
fn call_floor(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'floor' (number expected, got no value)".to_string(),
        ));
    }
    let v = get_number_arg(state, a, 0, "floor")?;
    match math_floor(&v) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.ceil(v) — 对应 C 的 math_ceil
fn call_ceil(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'ceil' (number expected, got no value)".to_string(),
        ));
    }
    let v = get_number_arg(state, a, 0, "ceil")?;
    match math_ceil(&v) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.fmod(a, b) — 对应 C 的 math_fmod
fn call_fmod(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to 'fmod' (number expected, got no value)",
            nargs + 1
        )));
    }
    let v1 = get_number_arg(state, a, 0, "fmod")?;
    let v2 = get_number_arg(state, a, 1, "fmod")?;
    match math_fmod(&v1, &v2) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.modf(x) — 对应 C 的 math_modf
fn call_modf(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'modf' (number expected, got no value)".to_string(),
        ));
    }
    let v = get_number_arg(state, a, 0, "modf")?;
    match math_modf(&v) {
        Ok((int_part, frac_part)) => {
            push_results(state, a, nresults, vec![int_part, frac_part]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.tointeger(v) — 对应 C 的 math_toint
fn call_tointeger(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'tointeger' (value expected)".to_string(),
        ));
    }
    let v = get_arg(state, a, 0);
    match math_tointeger(&v) {
        Some(n) => {
            push_single_result(state, a, nresults, TValue::Integer(n));
            Ok(())
        }
        None => {
            // 不可转换: 返回 nil (对应 C 的 luaL_pushfail)
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            Ok(())
        }
    }
}

/// math.ult(a, b) — 对应 C 的 math_ult
fn call_ult(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to 'ult' (integer expected, got no value)",
            nargs + 1
        )));
    }
    let a_val = get_int_arg(state, a, 0, "ult")?;
    let b_val = get_int_arg(state, a, 1, "ult")?;
    let result = math_ult(a_val, b_val);
    push_single_result(state, a, nresults, TValue::Boolean(result));
    Ok(())
}

/// math.frexp(x) — 对应 C 的 math_frexp
fn call_frexp(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'frexp' (number expected, got no value)".to_string(),
        ));
    }
    let v = get_number_arg(state, a, 0, "frexp")?;
    let x = to_float(&v).map_err(|msg| VmError::RuntimeError(msg))?;
    let (m, e) = math_frexp(x);
    push_results(state, a, nresults, vec![TValue::Float(m), TValue::Integer(e)]);
    Ok(())
}

/// math.ldexp(x, e) — 对应 C 的 math_ldexp
fn call_ldexp(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs < 2 {
        return Err(VmError::RuntimeError(format!(
            "bad argument #{} to 'ldexp' (value expected, got no value)",
            nargs + 1
        )));
    }
    let xv = get_number_arg(state, a, 0, "ldexp")?;
    let x = to_float(&xv).map_err(|msg| VmError::RuntimeError(msg))?;
    let e = get_int_arg(state, a, 1, "ldexp")?;
    let result = math_ldexp(x, e);
    push_single_result(state, a, nresults, TValue::Float(result));
    Ok(())
}

/// math.min(...) — 对应 C 的 math_min
fn call_min(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'min' (value expected)".to_string(),
        ));
    }
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    // 验证所有参数都是数字
    for (i, arg) in args.iter().enumerate() {
        if !matches!(arg, TValue::Integer(_) | TValue::Float(_)) {
            return Err(VmError::RuntimeError(format!(
                "bad argument #{} to 'min' (number expected, got {})",
                i + 1, arg.ty()
            )));
        }
    }
    match math_min(&args) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.max(...) — 对应 C 的 math_max
fn call_max(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'max' (value expected)".to_string(),
        ));
    }
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    // 验证所有参数都是数字
    for (i, arg) in args.iter().enumerate() {
        if !matches!(arg, TValue::Integer(_) | TValue::Float(_)) {
            return Err(VmError::RuntimeError(format!(
                "bad argument #{} to 'max' (number expected, got {})",
                i + 1, arg.ty()
            )));
        }
    }
    match math_max(&args) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.type(v) — 对应 C 的 math_type
fn call_type(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    if nargs == 0 {
        return Err(VmError::RuntimeError(
            "bad argument #1 to 'type' (value expected)".to_string(),
        ));
    }
    let v = get_arg(state, a, 0);
    match math_type(&v) {
        Some(name) => {
            push_single_result(state, a, nresults, TValue::Str(state.intern_str(name)));
            Ok(())
        }
        None => {
            // 非数字: 返回 nil (对应 C 的 luaL_pushfail)
            push_single_result(state, a, nresults, TValue::Nil(NilKind::Strict));
            Ok(())
        }
    }
}

/// math.random([low [, up]]) — 对应 C 的 math_random
fn call_random(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    let rand_state = state.math_random_state.as_mut().unwrap();
    match math_random(rand_state, &args) {
        Ok(result) => {
            push_single_result(state, a, nresults, result);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

/// math.randomseed([x [, y]]) — 对应 C 的 math_randomseed
fn call_randomseed(state: &mut LuaState, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError> {
    let args: Vec<TValue> = (0..nargs).map(|i| get_arg(state, a, i)).collect();
    let rand_state = state.math_random_state.as_mut().unwrap();
    match math_randomseed(rand_state, &args) {
        Ok((n1, n2)) => {
            push_results(state, a, nresults, vec![TValue::Integer(n1), TValue::Integer(n2)]);
            Ok(())
        }
        Err(msg) => Err(VmError::RuntimeError(msg)),
    }
}

// ============================================================================
// 打开数学库 — 对应 C 的 luaopen_math
// ============================================================================

/// 打开数学库, 注册所有数学函数和常量
///
/// 对应 C 源码 lmathlib.cpp 的 luaopen_math 函数:
/// 1. 创建数学库函数表并注册为全局变量 math
/// 2. 设置常量: pi, huge, maxinteger, mininteger
/// 3. 初始化随机数生成器状态
pub fn open_math_lib(state: &mut LuaState) {
    let mut lib = Table::new();

    // 注册所有数学库函数 (使用 LightUserData 标签)
    let register = |lib: &mut Table, name: &str, tag: usize| {
        let key = TValue::Str(state.intern_str(name));
        lib.set(key, TValue::LightUserData(tag as *mut std::ffi::c_void));
    };

    register(&mut lib, "abs", MATH_ABS);
    register(&mut lib, "acos", MATH_ACOS);
    register(&mut lib, "asin", MATH_ASIN);
    register(&mut lib, "atan", MATH_ATAN);
    register(&mut lib, "ceil", MATH_CEIL);
    register(&mut lib, "cos", MATH_COS);
    register(&mut lib, "deg", MATH_DEG);
    register(&mut lib, "exp", MATH_EXP);
    register(&mut lib, "tointeger", MATH_TOINTEGER);
    register(&mut lib, "floor", MATH_FLOOR);
    register(&mut lib, "fmod", MATH_FMOD);
    register(&mut lib, "frexp", MATH_FREXP);
    register(&mut lib, "ult", MATH_ULT);
    register(&mut lib, "ldexp", MATH_LDEXP);
    register(&mut lib, "log", MATH_LOG);
    register(&mut lib, "max", MATH_MAX);
    register(&mut lib, "min", MATH_MIN);
    register(&mut lib, "modf", MATH_MODF);
    register(&mut lib, "rad", MATH_RAD);
    register(&mut lib, "sin", MATH_SIN);
    register(&mut lib, "sqrt", MATH_SQRT);
    register(&mut lib, "tan", MATH_TAN);
    register(&mut lib, "type", MATH_TYPE);
    register(&mut lib, "random", MATH_RANDOM);
    register(&mut lib, "randomseed", MATH_RANDOMSEED);

    // 设置常量 (对应 C 的 lua_pushnumber/lua_pushinteger + lua_setfield)
    lib.set(TValue::Str(state.intern_str("pi")), TValue::Float(PI));
    lib.set(TValue::Str(state.intern_str("huge")), TValue::Float(HUGE));
    lib.set(
        TValue::Str(state.intern_str("maxinteger")),
        TValue::Integer(MAX_INTEGER),
    );
    lib.set(
        TValue::Str(state.intern_str("mininteger")),
        TValue::Integer(MIN_INTEGER),
    );

    // 注册为全局变量 math
    let key = TValue::Str(state.intern_str("math"));
    state.globals.set(key, TValue::Table(lib));

    // 初始化随机数生成器状态 (对应 C 的 setrandfunc)
    // 使用时间种子初始化
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    if state.math_random_state.is_none() {
        state.math_random_state = Some(Box::new(RandState::new()));
    }
    let rand_state = state.math_random_state.as_mut().unwrap();
    rand_state.setseed(seed, 0);
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_str(s: &str) -> TValue {
        TValue::Str(crate::strings::LuaString::Short(Arc::new(
            crate::strings::ShortString {
                hash: 0,
                contents: s.to_string(),
            },
        )))
    }

    // ========================================================================
    // 常量测试
    // ========================================================================

    #[test]
    fn test_constants() {
        assert!((PI - std::f64::consts::PI).abs() < 1e-15);
        assert!(HUGE.is_infinite() && HUGE > 0.0);
        assert_eq!(MAX_INTEGER, i64::MAX);
        assert_eq!(MIN_INTEGER, i64::MIN);
    }

    // ========================================================================
    // math_abs 测试
    // ========================================================================

    #[test]
    fn test_math_abs_integer() {
        assert_eq!(math_abs(&TValue::Integer(42)).unwrap(), TValue::Integer(42));
        assert_eq!(math_abs(&TValue::Integer(-42)).unwrap(), TValue::Integer(42));
        assert_eq!(math_abs(&TValue::Integer(0)).unwrap(), TValue::Integer(0));
    }

    #[test]
    fn test_math_abs_float() {
        assert_eq!(math_abs(&TValue::Float(3.14)).unwrap(), TValue::Float(3.14));
        assert_eq!(math_abs(&TValue::Float(-3.14)).unwrap(), TValue::Float(3.14));
        assert_eq!(math_abs(&TValue::Float(0.0)).unwrap(), TValue::Float(0.0));
    }

    #[test]
    fn test_math_abs_invalid() {
        assert!(math_abs(&TValue::Boolean(true)).is_err());
        assert!(math_abs(&make_str("abc")).is_err());
    }

    // ========================================================================
    // 三角函数测试
    // ========================================================================

    #[test]
    fn test_math_sin() {
        assert!((math_sin(0.0) - 0.0).abs() < 1e-15);
        assert!((math_sin(PI / 2.0) - 1.0).abs() < 1e-15);
        assert!((math_sin(PI) - 0.0).abs() < 1e-15);
    }

    #[test]
    fn test_math_cos() {
        assert!((math_cos(0.0) - 1.0).abs() < 1e-15);
        assert!((math_cos(PI / 2.0) - 0.0).abs() < 1e-15);
        assert!((math_cos(PI) - (-1.0)).abs() < 1e-15);
    }

    #[test]
    fn test_math_tan() {
        assert!((math_tan(0.0) - 0.0).abs() < 1e-15);
        assert!((math_tan(PI / 4.0) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn test_math_asin() {
        assert!((math_asin(0.0) - 0.0).abs() < 1e-15);
        assert!((math_asin(1.0) - PI / 2.0).abs() < 1e-15);
    }

    #[test]
    fn test_math_acos() {
        assert!((math_acos(0.0) - PI / 2.0).abs() < 1e-15);
        assert!((math_acos(1.0) - 0.0).abs() < 1e-15);
        assert!((math_acos(-1.0) - PI).abs() < 1e-15);
    }

    #[test]
    fn test_math_atan() {
        assert!((math_atan(0.0, None) - 0.0).abs() < 1e-15);
        assert!((math_atan(1.0, None) - PI / 4.0).abs() < 1e-15);
        // atan2(1, 1) = PI/4
        assert!((math_atan(1.0, Some(1.0)) - PI / 4.0).abs() < 1e-15);
        // atan2(1, 0) = PI/2
        assert!((math_atan(1.0, Some(0.0)) - PI / 2.0).abs() < 1e-15);
    }

    // ========================================================================
    // floor/ceil 测试
    // ========================================================================

    #[test]
    fn test_math_floor_integer() {
        assert_eq!(math_floor(&TValue::Integer(42)).unwrap(), TValue::Integer(42));
        assert_eq!(math_floor(&TValue::Integer(-42)).unwrap(), TValue::Integer(-42));
    }

    #[test]
    fn test_math_floor_float() {
        assert_eq!(math_floor(&TValue::Float(3.7)).unwrap(), TValue::Integer(3));
        assert_eq!(math_floor(&TValue::Float(-3.7)).unwrap(), TValue::Integer(-4));
        assert_eq!(math_floor(&TValue::Float(3.0)).unwrap(), TValue::Integer(3));
        // 大浮点数无法无损转为整数
        let big = 1e20;
        assert_eq!(math_floor(&TValue::Float(big)).unwrap(), TValue::Float(big.floor()));
    }

    #[test]
    fn test_math_ceil_integer() {
        assert_eq!(math_ceil(&TValue::Integer(42)).unwrap(), TValue::Integer(42));
        assert_eq!(math_ceil(&TValue::Integer(-42)).unwrap(), TValue::Integer(-42));
    }

    #[test]
    fn test_math_ceil_float() {
        assert_eq!(math_ceil(&TValue::Float(3.2)).unwrap(), TValue::Integer(4));
        assert_eq!(math_ceil(&TValue::Float(-3.2)).unwrap(), TValue::Integer(-3));
        assert_eq!(math_ceil(&TValue::Float(3.0)).unwrap(), TValue::Integer(3));
    }

    // ========================================================================
    // fmod 测试
    // ========================================================================

    #[test]
    fn test_math_fmod_integer() {
        assert_eq!(
            math_fmod(&TValue::Integer(10), &TValue::Integer(3)).unwrap(),
            TValue::Integer(1)
        );
        assert_eq!(
            math_fmod(&TValue::Integer(-10), &TValue::Integer(3)).unwrap(),
            TValue::Integer(-1)
        );
        assert_eq!(
            math_fmod(&TValue::Integer(10), &TValue::Integer(-3)).unwrap(),
            TValue::Integer(1)
        );
    }

    #[test]
    fn test_math_fmod_integer_zero() {
        assert!(math_fmod(&TValue::Integer(10), &TValue::Integer(0)).is_err());
    }

    #[test]
    fn test_math_fmod_integer_neg_one() {
        // 除以 -1 时返回 0 (避免溢出)
        assert_eq!(
            math_fmod(&TValue::Integer(i64::MIN), &TValue::Integer(-1)).unwrap(),
            TValue::Integer(0)
        );
    }

    #[test]
    fn test_math_fmod_float() {
        let result = math_fmod(&TValue::Float(10.5), &TValue::Float(3.0)).unwrap();
        assert!(matches!(result, TValue::Float(f) if (f - 1.5).abs() < 1e-15));
    }

    #[test]
    fn test_math_fmod_mixed() {
        let result = math_fmod(&TValue::Integer(10), &TValue::Float(3.0)).unwrap();
        assert!(matches!(result, TValue::Float(f) if (f - 1.0).abs() < 1e-15));
    }

    // ========================================================================
    // modf 测试
    // ========================================================================

    #[test]
    fn test_math_modf_integer() {
        let (int, frac) = math_modf(&TValue::Integer(42)).unwrap();
        assert_eq!(int, TValue::Integer(42));
        assert_eq!(frac, TValue::Float(0.0));
    }

    #[test]
    fn test_math_modf_float_positive() {
        let (int, frac) = math_modf(&TValue::Float(3.14)).unwrap();
        assert_eq!(int, TValue::Integer(3));
        assert!(matches!(frac, TValue::Float(f) if (f - 0.14).abs() < 1e-15));
    }

    #[test]
    fn test_math_modf_float_negative() {
        let (int, frac) = math_modf(&TValue::Float(-3.14)).unwrap();
        assert_eq!(int, TValue::Integer(-3));
        assert!(matches!(frac, TValue::Float(f) if (f - (-0.14)).abs() < 1e-15));
    }

    #[test]
    fn test_math_modf_float_whole() {
        let (int, frac) = math_modf(&TValue::Float(3.0)).unwrap();
        assert_eq!(int, TValue::Integer(3));
        assert_eq!(frac, TValue::Float(0.0));
    }

    // ========================================================================
    // sqrt/exp/log 测试
    // ========================================================================

    #[test]
    fn test_math_sqrt() {
        assert!((math_sqrt(4.0) - 2.0).abs() < 1e-15);
        assert!((math_sqrt(2.0) - std::f64::consts::SQRT_2).abs() < 1e-15);
        assert!(math_sqrt(-1.0).is_nan());
    }

    #[test]
    fn test_math_exp() {
        assert!((math_exp(0.0) - 1.0).abs() < 1e-15);
        assert!((math_exp(1.0) - std::f64::consts::E).abs() < 1e-15);
    }

    #[test]
    fn test_math_log_natural() {
        assert!((math_log(1.0, None) - 0.0).abs() < 1e-15);
        assert!((math_log(std::f64::consts::E, None) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn test_math_log_base() {
        assert!((math_log(8.0, Some(2.0)) - 3.0).abs() < 1e-15);
        assert!((math_log(100.0, Some(10.0)) - 2.0).abs() < 1e-15);
        assert!((math_log(1000.0, Some(10.0)) - 3.0).abs() < 1e-15);
    }

    // ========================================================================
    // deg/rad 测试
    // ========================================================================

    #[test]
    fn test_math_deg() {
        assert!((math_deg(PI) - 180.0).abs() < 1e-15);
        assert!((math_deg(PI / 2.0) - 90.0).abs() < 1e-15);
        assert!((math_deg(0.0) - 0.0).abs() < 1e-15);
    }

    #[test]
    fn test_math_rad() {
        assert!((math_rad(180.0) - PI).abs() < 1e-15);
        assert!((math_rad(90.0) - PI / 2.0).abs() < 1e-15);
        assert!((math_rad(0.0) - 0.0).abs() < 1e-15);
    }

    // ========================================================================
    // tointeger/ult 测试
    // ========================================================================

    #[test]
    fn test_math_tointeger() {
        assert_eq!(math_tointeger(&TValue::Integer(42)), Some(42));
        assert_eq!(math_tointeger(&TValue::Float(42.0)), Some(42));
        assert_eq!(math_tointeger(&TValue::Float(42.5)), None);
        assert_eq!(math_tointeger(&TValue::Boolean(true)), None);
        assert_eq!(math_tointeger(&make_str("42")), Some(42));
    }

    #[test]
    fn test_math_ult() {
        assert!(math_ult(1, 2));
        assert!(!math_ult(2, 1));
        assert!(!math_ult(1, 1));
        // 无符号比较: -1 作为无符号是 u64::MAX, 大于 1
        assert!(!math_ult(-1, 1));
        assert!(math_ult(1, -1));
    }

    // ========================================================================
    // frexp/ldexp 测试
    // ========================================================================

    #[test]
    fn test_math_frexp() {
        let (m, e) = math_frexp(0.0);
        assert_eq!(m, 0.0);
        assert_eq!(e, 0);

        let (m, e) = math_frexp(1.0);
        assert!((m - 0.5).abs() < 1e-15);
        assert_eq!(e, 1);

        let (m, e) = math_frexp(4.0);
        assert!((m - 0.5).abs() < 1e-15);
        assert_eq!(e, 3);
    }

    #[test]
    fn test_math_ldexp() {
        assert!((math_ldexp(0.5, 1) - 1.0).abs() < 1e-15);
        assert!((math_ldexp(0.5, 3) - 4.0).abs() < 1e-15);
        assert!((math_ldexp(1.0, 0) - 1.0).abs() < 1e-15);
        assert!((math_ldexp(1.0, 10) - 1024.0).abs() < 1e-15);
    }

    #[test]
    fn test_frexp_ldexp_roundtrip() {
        let x = 3.14;
        let (m, e) = math_frexp(x);
        let restored = math_ldexp(m, e);
        assert!((restored - x).abs() < 1e-15);
    }

    // ========================================================================
    // min/max 测试
    // ========================================================================

    #[test]
    fn test_math_min() {
        let args = vec![TValue::Integer(3), TValue::Integer(1), TValue::Integer(2)];
        assert_eq!(math_min(&args).unwrap(), TValue::Integer(1));
    }

    #[test]
    fn test_math_min_float() {
        let args = vec![TValue::Float(3.14), TValue::Float(1.41), TValue::Float(2.71)];
        assert_eq!(math_min(&args).unwrap(), TValue::Float(1.41));
    }

    #[test]
    fn test_math_min_mixed() {
        let args = vec![TValue::Integer(3), TValue::Float(1.5)];
        assert_eq!(math_min(&args).unwrap(), TValue::Float(1.5));
    }

    #[test]
    fn test_math_min_single() {
        let args = vec![TValue::Integer(42)];
        assert_eq!(math_min(&args).unwrap(), TValue::Integer(42));
    }

    #[test]
    fn test_math_min_empty() {
        let args: Vec<TValue> = vec![];
        assert!(math_min(&args).is_err());
    }

    #[test]
    fn test_math_max() {
        let args = vec![TValue::Integer(1), TValue::Integer(3), TValue::Integer(2)];
        assert_eq!(math_max(&args).unwrap(), TValue::Integer(3));
    }

    #[test]
    fn test_math_max_mixed() {
        let args = vec![TValue::Integer(1), TValue::Float(2.5)];
        assert_eq!(math_max(&args).unwrap(), TValue::Float(2.5));
    }

    #[test]
    fn test_math_max_empty() {
        let args: Vec<TValue> = vec![];
        assert!(math_max(&args).is_err());
    }

    // ========================================================================
    // math_type 测试
    // ========================================================================

    #[test]
    fn test_math_type() {
        assert_eq!(math_type(&TValue::Integer(42)), Some("integer"));
        assert_eq!(math_type(&TValue::Float(3.14)), Some("float"));
        assert_eq!(math_type(&TValue::Boolean(true)), None);
        assert_eq!(math_type(&TValue::Nil(NilKind::Strict)), None);
        assert_eq!(math_type(&make_str("42")), None);
    }

    // ========================================================================
    // 随机数生成器测试
    // ========================================================================

    #[test]
    fn test_rand_state_setseed() {
        let mut state1 = RandState::new();
        let mut state2 = RandState::new();
        state1.setseed(42, 0);
        state2.setseed(42, 0);

        // 相同种子应产生相同序列
        for _ in 0..10 {
            assert_eq!(state1.nextrand(), state2.nextrand());
        }
    }

    #[test]
    fn test_rand_state_different_seeds() {
        let mut state1 = RandState::new();
        let mut state2 = RandState::new();
        state1.setseed(42, 0);
        state2.setseed(43, 0);

        // 不同种子应产生不同序列
        let mut diff = false;
        for _ in 0..10 {
            if state1.nextrand() != state2.nextrand() {
                diff = true;
                break;
            }
        }
        assert!(diff);
    }

    #[test]
    fn test_rand_state_i2d_range() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        for _ in 0..100 {
            let r = state.nextrand();
            let f = RandState::i2d(r);
            assert!(f >= 0.0 && f < 1.0, "i2d returned {} which is out of [0, 1)", f);
        }
    }

    #[test]
    fn test_rand_state_project() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        for _ in 0..100 {
            let ran = state.nextrand();
            let p = state.project(ran, 10);
            assert!(p <= 10, "project returned {} which is > 10", p);
        }
    }

    #[test]
    fn test_math_random_no_args() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        let result = math_random(&mut state, &[]).unwrap();
        match result {
            TValue::Float(f) => {
                assert!(f >= 0.0 && f < 1.0);
            }
            _ => panic!("expected float result"),
        }
    }

    #[test]
    fn test_math_random_single_arg() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        for _ in 0..100 {
            let result = math_random(&mut state, &[TValue::Integer(6)]).unwrap();
            match result {
                TValue::Integer(n) => {
                    assert!(n >= 1 && n <= 6, "random(6) returned {}", n);
                }
                _ => panic!("expected integer result"),
            }
        }
    }

    #[test]
    fn test_math_random_two_args() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        for _ in 0..100 {
            let result = math_random(&mut state, &[TValue::Integer(10), TValue::Integer(20)]).unwrap();
            match result {
                TValue::Integer(n) => {
                    assert!(n >= 10 && n <= 20, "random(10, 20) returned {}", n);
                }
                _ => panic!("expected integer result"),
            }
        }
    }

    #[test]
    fn test_math_random_zero_arg() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        let result = math_random(&mut state, &[TValue::Integer(0)]).unwrap();
        assert!(matches!(result, TValue::Integer(_)));
    }

    #[test]
    fn test_math_random_empty_interval() {
        let mut state = RandState::new();
        state.setseed(42, 0);
        let result = math_random(&mut state, &[TValue::Integer(5), TValue::Integer(3)]);
        assert!(result.is_err());
    }

    #[test]
    fn test_math_randomseed() {
        let mut state = RandState::new();
        let (n1, n2) = math_randomseed(&mut state, &[TValue::Integer(42)]).unwrap();
        assert_eq!(n1, 42);
        assert_eq!(n2, 0);
    }

    #[test]
    fn test_math_randomseed_two_args() {
        let mut state = RandState::new();
        let (n1, n2) = math_randomseed(
            &mut state,
            &[TValue::Integer(42), TValue::Integer(99)],
        ).unwrap();
        assert_eq!(n1, 42);
        assert_eq!(n2, 99);
    }

    #[test]
    fn test_math_random_reproducible() {
        let mut state1 = RandState::new();
        let mut state2 = RandState::new();
        math_randomseed(&mut state1, &[TValue::Integer(123)]).unwrap();
        math_randomseed(&mut state2, &[TValue::Integer(123)]).unwrap();

        // 相同种子应产生相同随机序列
        for _ in 0..10 {
            let r1 = math_random(&mut state1, &[TValue::Integer(100)]).unwrap();
            let r2 = math_random(&mut state2, &[TValue::Integer(100)]).unwrap();
            assert_eq!(r1, r2);
        }
    }

    // ========================================================================
    // is_math_tag 测试
    // ========================================================================

    #[test]
    fn test_is_math_tag() {
        assert!(is_math_tag(MATH_ABS));
        assert!(is_math_tag(MATH_RANDOM));
        assert!(is_math_tag(MATH_RANDOMSEED));
        assert!(is_math_tag(299));
        assert!(!is_math_tag(199));
        assert!(!is_math_tag(300));
        assert!(!is_math_tag(0));
        assert!(!is_math_tag(100));
    }

    // ========================================================================
    // math_function_name 测试
    // ========================================================================

    #[test]
    fn test_math_function_name() {
        assert_eq!(math_function_name(MATH_ABS), Some("abs"));
        assert_eq!(math_function_name(MATH_SIN), Some("sin"));
        assert_eq!(math_function_name(MATH_RANDOM), Some("random"));
        assert_eq!(math_function_name(MATH_RANDOMSEED), Some("randomseed"));
        assert_eq!(math_function_name(999), None);
    }

    // ========================================================================
    // open_math_lib 测试
    // ========================================================================

    #[test]
    fn test_open_math_lib_registers_global() {
        let mut state = LuaState::new();
        open_math_lib(&mut state);
        let key = TValue::Str(state.intern_str("math"));
        let val = state.globals.get(&key);
        assert!(val.is_some(), "math global must be registered");
        assert!(matches!(val, Some(TValue::Table(_))));
    }

    #[test]
    fn test_open_math_lib_has_all_functions() {
        let mut state = LuaState::new();
        open_math_lib(&mut state);

        // 获取 math 表
        let math_key = TValue::Str(state.intern_str("math"));
        let math_table = match state.globals.get(&math_key) {
            Some(TValue::Table(t)) => t.clone(),
            _ => panic!("math table not found"),
        };

        // 验证所有函数
        for name in &[
            "abs", "acos", "asin", "atan", "ceil", "cos", "deg", "exp",
            "tointeger", "floor", "fmod", "frexp", "ult", "ldexp", "log",
            "max", "min", "modf", "rad", "sin", "sqrt", "tan", "type",
            "random", "randomseed",
        ] {
            let key = TValue::Str(state.intern_str(name));
            assert!(
                math_table.get(&key).is_some(),
                "math.{} must be registered",
                name
            );
        }
    }

    #[test]
    fn test_open_math_lib_has_constants() {
        let mut state = LuaState::new();
        open_math_lib(&mut state);

        let math_key = TValue::Str(state.intern_str("math"));
        let math_table = match state.globals.get(&math_key) {
            Some(TValue::Table(t)) => t.clone(),
            _ => panic!("math table not found"),
        };

        // 验证常量
        let pi_key = TValue::Str(state.intern_str("pi"));
        match math_table.get(&pi_key) {
            Some(TValue::Float(f)) => assert!((f - PI).abs() < 1e-15),
            _ => panic!("math.pi not found or wrong type"),
        }

        let huge_key = TValue::Str(state.intern_str("huge"));
        match math_table.get(&huge_key) {
            Some(TValue::Float(f)) => assert!(f.is_infinite() && f > 0.0),
            _ => panic!("math.huge not found or wrong type"),
        }

        let maxint_key = TValue::Str(state.intern_str("maxinteger"));
        match math_table.get(&maxint_key) {
            Some(TValue::Integer(n)) => assert_eq!(n, i64::MAX),
            _ => panic!("math.maxinteger not found or wrong type"),
        }

        let minint_key = TValue::Str(state.intern_str("mininteger"));
        match math_table.get(&minint_key) {
            Some(TValue::Integer(n)) => assert_eq!(n, i64::MIN),
            _ => panic!("math.mininteger not found or wrong type"),
        }
    }

    // ========================================================================
    // call_math_function 测试
    // ========================================================================

    #[test]
    fn test_call_math_function_abs() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_ABS as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(-42));
        call_math_function(MATH_ABS, &mut state, 0, 1, 1).unwrap();
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer 42"),
        }
    }

    #[test]
    fn test_call_math_function_floor() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_FLOOR as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(3.7));
        call_math_function(MATH_FLOOR, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 3),
            _ => panic!("expected integer 3"),
        }
    }

    #[test]
    fn test_call_math_function_max() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_MAX as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(5));
        state.stack.push(TValue::Integer(3));
        call_math_function(MATH_MAX, &mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 5),
            _ => panic!("expected integer 5"),
        }
    }

    #[test]
    fn test_call_math_function_min() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_MIN as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(5));
        state.stack.push(TValue::Integer(3));
        call_math_function(MATH_MIN, &mut state, 0, 3, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 1),
            _ => panic!("expected integer 1"),
        }
    }

    #[test]
    fn test_call_math_function_type() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_TYPE as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_math_function(MATH_TYPE, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "integer"),
            _ => panic!("expected string 'integer'"),
        }
    }

    #[test]
    fn test_call_math_function_type_float() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_TYPE as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(3.14));
        call_math_function(MATH_TYPE, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Str(s) => assert_eq!(s.as_str(), "float"),
            _ => panic!("expected string 'float'"),
        }
    }

    #[test]
    fn test_call_math_function_tointeger() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_TOINTEGER as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(42.0));
        call_math_function(MATH_TOINTEGER, &mut state, 0, 1, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer 42"),
        }
    }

    #[test]
    fn test_call_math_function_tointeger_fail() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_TOINTEGER as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(42.5));
        call_math_function(MATH_TOINTEGER, &mut state, 0, 1, 1).unwrap();
        assert!(matches!(state.stack[0], TValue::Nil(_)));
    }

    #[test]
    fn test_call_math_function_ult() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_ULT as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(2));
        call_math_function(MATH_ULT, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Boolean(b) => assert!(*b),
            _ => panic!("expected boolean true"),
        }
    }

    #[test]
    fn test_call_math_function_modf() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_MODF as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(3.14));
        call_math_function(MATH_MODF, &mut state, 0, 1, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 3),
            _ => panic!("expected integer 3"),
        }
        match &state.stack[1] {
            TValue::Float(f) => assert!((f - 0.14).abs() < 1e-15),
            _ => panic!("expected float 0.14"),
        }
    }

    #[test]
    fn test_call_math_function_frexp() {
        let mut state = LuaState::new();
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_FREXP as *mut std::ffi::c_void));
        state.stack.push(TValue::Float(4.0));
        call_math_function(MATH_FREXP, &mut state, 0, 1, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Float(f) => assert!((f - 0.5).abs() < 1e-15),
            _ => panic!("expected float 0.5"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 3),
            _ => panic!("expected integer 3"),
        }
    }

    #[test]
    fn test_call_math_function_randomseed() {
        let mut state = LuaState::new();
        open_math_lib(&mut state);
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_RANDOMSEED as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(42));
        call_math_function(MATH_RANDOMSEED, &mut state, 0, 1, -1).unwrap();
        assert_eq!(state.stack.len(), 2);
        match &state.stack[0] {
            TValue::Integer(n) => assert_eq!(*n, 42),
            _ => panic!("expected integer 42"),
        }
        match &state.stack[1] {
            TValue::Integer(n) => assert_eq!(*n, 0),
            _ => panic!("expected integer 0"),
        }
    }

    #[test]
    fn test_call_math_function_random() {
        let mut state = LuaState::new();
        open_math_lib(&mut state);
        state.stack.clear();
        state.stack.push(TValue::LightUserData(MATH_RANDOM as *mut std::ffi::c_void));
        state.stack.push(TValue::Integer(1));
        state.stack.push(TValue::Integer(100));
        call_math_function(MATH_RANDOM, &mut state, 0, 2, 1).unwrap();
        match &state.stack[0] {
            TValue::Integer(n) => {
                assert!(*n >= 1 && *n <= 100, "random(1, 100) returned {}", n);
            }
            _ => panic!("expected integer result"),
        }
    }

    #[test]
    fn test_call_math_function_unknown_tag() {
        let mut state = LuaState::new();
        let result = call_math_function(999, &mut state, 0, 0, 0);
        assert!(result.is_err());
    }
}
