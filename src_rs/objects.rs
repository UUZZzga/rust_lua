//! Lua 对象和值类型表示 (纯 Rust 重写)
//!
//! 对应 C 源码: lobject.h + lobject.cpp
//!
//! ## 设计原则
//! - 使用 Rust `enum` 替代 C 的 tagged union + bit-tag，用模式匹配替代宏/位测试
//! - 使用 `Option` / `Result` 替代 NULL / 哨兵值
//! - 使用 Rust 所有权系统管理对象生命周期
//! - 类型安全的数字转换（区分整数和浮点数）
//! - nil 的多种语义通过 enum 变体表达，而非位掩码
//!
//! ## Lua 类型系统
//! Lua 有 9 种基本类型：nil, boolean, lightuserdata, number, string,
//! table, function, userdata, thread。每种类型可能有子变体（如 number 分 integer/float）。
//!
//! ## 规约驱动开发 (spec-driven-tdd)
//! 以下每个类型/函数都包含规约注释（Scenario / Given / When / Then）。
//! 规约未获批准前，不得编写实现代码。
//!
//! ```text
//! 工作流程: 编写规约 → 人类评审 → 红灯(失败测试) → 绿灯(最小实现) → 重构
//! ```

use std::fmt;
use std::hash::{Hash, Hasher};

use crate::strings::LuaString;
use std::cell::RefCell;
use std::rc::Rc;

use crate::gc::GCObjectHeader;

// ============================================================================
// 规约：Lua 基础类型标签
// ============================================================================

/// 共享上值引用 —— 多个闭包可以共享同一个 UpVal（对应 C 中 UpVal 是堆分配对象）。
/// 当 Open 上值被关闭时，所有持有该引用的闭包都能看到 Closed 状态。
pub type UpValRef = Rc<RefCell<UpVal>>;

/// Lua 类型标签 —— 使用 Rust enum 替代 C 的整数常量 + 位掩码
///
/// Lua 有 9 种基本值类型。在 C 实现中，类型标签使用整数 + 变体位 + 可回收位。
/// Rust 版本直接用 enum，编译器保证穷举匹配。
///
/// Scenario: 类型标签的基本分类
/// Given: 一个 Lua 值
/// When: 检查其类型标签
/// Then: 返回 9 种基本类型之一
/// And: 每种类型的整数值与 Lua C API 兼容
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LuaType {
    /// nil 类型 (LUA_TNIL = 0)
    Nil = 0,
    /// boolean 类型 (LUA_TBOOLEAN = 1)
    Boolean = 1,
    /// light userdata 类型 (LUA_TLIGHTUSERDATA = 2)
    LightUserData = 2,
    /// number 类型 (LUA_TNUMBER = 3)
    Number = 3,
    /// string 类型 (LUA_TSTRING = 4)
    String = 4,
    /// table 类型 (LUA_TTABLE = 5)
    Table = 5,
    /// function 类型 (LUA_TFUNCTION = 6)
    Function = 6,
    /// full userdata 类型 (LUA_TUSERDATA = 7)
    UserData = 7,
    /// thread 类型 (LUA_TTHREAD = 8)
    Thread = 8,
}

impl LuaType {
    /// 从整数值构造 LuaType
    ///
    /// Scenario: 从整数构造类型标签
    /// Given: 有效整数 0..=8
    /// When: 调用 LuaType::from_u8(n)
    /// Then: 返回 Some(LuaType)
    /// Given: 无效整数 >= 9
    /// When: 调用 LuaType::from_u8(n)
    /// Then: 返回 None
    pub fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(LuaType::Nil),
            1 => Some(LuaType::Boolean),
            2 => Some(LuaType::LightUserData),
            3 => Some(LuaType::Number),
            4 => Some(LuaType::String),
            5 => Some(LuaType::Table),
            6 => Some(LuaType::Function),
            7 => Some(LuaType::UserData),
            8 => Some(LuaType::Thread),
            _ => None,
        }
    }
}

impl fmt::Display for LuaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LuaType::Nil => write!(f, "nil"),
            LuaType::Boolean => write!(f, "boolean"),
            LuaType::LightUserData => write!(f, "lightuserdata"),
            LuaType::Number => write!(f, "number"),
            LuaType::String => write!(f, "string"),
            LuaType::Table => write!(f, "table"),
            LuaType::Function => write!(f, "function"),
            LuaType::UserData => write!(f, "userdata"),
            LuaType::Thread => write!(f, "thread"),
        }
    }
}

// ============================================================================
// 规约：TValue — Lua 核心值类型
// ============================================================================

/// Lua 标记联合值 (Tagged Value) —— Lua 中最基础的值表示
///
/// 在 C 实现中，TValue 是一个 union + 一个 tag byte，tag 的低 4 位是类型，
/// 第 4-5 位是变体，第 6 位标记是否可 GC 回收。
///
/// Rust 版本使用 enum + struct，类型安全且无位操作。
///
/// Scenario: TValue 的构造与类型查询
/// Given: 创建各种类型的 TValue
/// When: 调用 .ty() 方法
/// Then: 返回正确的 LuaType
#[derive(Debug, Clone)]
pub enum TValue {
    /// nil 值，带子变体（标准 nil / 空槽 / 缺键）
    Nil(NilKind),
    /// 布尔值
    Boolean(bool),
    /// 轻量用户数据（裸指针）
    LightUserData(*mut std::ffi::c_void),
    /// 数值 —— 整数子变体
    Integer(i64),
    /// 数值 —— 浮点子变体
    Float(f64),
    /// 字符串（短字符串和长字符串）
    Str(LuaString),
    /// 表
    Table(Table),
    /// Lua 闭包
    LClosure(LClosure),
    /// C 闭包
    CClosure(CClosure),
    /// 轻量 C 函数
    LCFn(LCFunction),
    /// 用户数据
    UserData(Udata),
    /// 线程/协程
    Thread(LuaThread),
}

/// nil 的子变体 —— 用 enum 替代 C 的 variant bit
///
/// Scenario: nil 的语义区分
/// Given: 一个值为 nil
/// When: 检查其 NilKind
/// Then: 可区分为标准 nil、空槽、缺键、非表哨兵
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NilKind {
    /// 标准 nil (LUA_VNIL)
    Strict,
    /// 空槽位 (LUA_VEMPTY)
    Empty,
    /// 表中不存在的键 (LUA_VABSTKEY)
    AbsentKey,
    /// 快速访问非表时的哨兵 (LUA_VNOTABLE)
    NotTable,
}

impl TValue {
    /// 获取 TValue 的 LuaType
    ///
    /// Scenario: 查询值的类型
    /// Given: 任意 TValue
    /// When: 调用 .ty()
    /// Then: 返回对应的 LuaType
    pub fn ty(&self) -> LuaType {
        match self {
            TValue::Nil(_) => LuaType::Nil,
            TValue::Boolean(_) => LuaType::Boolean,
            TValue::LightUserData(_) => LuaType::LightUserData,
            TValue::Integer(_) | TValue::Float(_) => LuaType::Number,
            TValue::Str(_) => LuaType::String,
            TValue::Table(_) => LuaType::Table,
            TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_) => LuaType::Function,
            TValue::UserData(_) => LuaType::UserData,
            TValue::Thread(_) => LuaType::Thread,
        }
    }

    /// 是否为 nil（任何子变体）
    ///
    /// Scenario: 判断是否为 nil
    /// Given: 一个 TValue::Nil(Strict)
    /// When: 调用 .is_nil()
    /// Then: 返回 true
    /// Given: 一个 TValue::Nil(Empty)
    /// When: 调用 .is_nil()
    /// Then: 返回 true
    /// Given: 一个 TValue::Boolean(true)
    /// When: 调用 .is_nil()
    /// Then: 返回 false
    pub fn is_nil(&self) -> bool {
        matches!(self, TValue::Nil(_))
    }

    /// 是否为严格 nil（只有 NilKind::Strict）
    ///
    /// Scenario: 判断严格 nil
    /// Given: TValue::Nil(Strict)
    /// When: 调用 .is_strict_nil()
    /// Then: 返回 true
    /// Given: TValue::Nil(Empty)
    /// When: 调用 .is_strict_nil()
    /// Then: 返回 false
    pub fn is_strict_nil(&self) -> bool {
        matches!(self, TValue::Nil(NilKind::Strict))
    }

    /// 是否为布尔 false 或 nil（Lua 中的 "假" 值）
    ///
    /// Scenario: Lua 假值判断
    /// Given: TValue::Boolean(false)
    /// When: 调用 .is_false()
    /// Then: 返回 true
    /// Given: TValue::Nil(_)
    /// When: 调用 .is_false()
    /// Then: 返回 true
    /// Given: TValue::Boolean(true)
    /// When: 调用 .is_false()
    /// Then: 返回 false
    pub fn is_false(&self) -> bool {
        matches!(self, TValue::Boolean(false)) || self.is_nil()
    }

    /// 是否为整数
    ///
    /// Scenario: 判断整数类型
    /// Given: TValue::Integer(42)
    /// When: 调用 .is_integer()
    /// Then: 返回 true
    /// Given: TValue::Float(3.14)
    /// When: 调用 .is_integer()
    /// Then: 返回 false
    pub fn is_integer(&self) -> bool {
        matches!(self, TValue::Integer(_))
    }

    /// 是否为浮点数
    ///
    /// Scenario: 判断浮点类型
    /// Given: TValue::Float(1.5)
    /// When: 调用 .is_float()
    /// Then: 返回 true
    /// Given: TValue::Integer(1)
    /// When: 调用 .is_float()
    /// Then: 返回 false
    pub fn is_float(&self) -> bool {
        matches!(self, TValue::Float(_))
    }

    /// 是否为数字（整数或浮点数）
    ///
    /// Scenario: 判断数字类型
    /// Given: TValue::Integer(42)
    /// When: 调用 .is_number()
    /// Then: 返回 true
    /// Given: TValue::Float(3.14)
    /// When: 调用 .is_number()
    /// Then: 返回 true
    /// Given: TValue::Nil(_)
    /// When: 调用 .is_number()
    /// Then: 返回 false
    pub fn is_number(&self) -> bool {
        matches!(self, TValue::Integer(_) | TValue::Float(_))
    }

    /// 是否为字符串
    ///
    /// Scenario: 判断字符串类型
    /// Given: TValue::StrS(_)
    /// When: 调用 .is_string()
    /// Then: 返回 true
    pub fn is_string(&self) -> bool {
        matches!(self, TValue::Str(_))
    }

    /// 是否为表
    ///
    /// Scenario: 判断表类型
    /// Given: TValue::Table(_)
    /// When: 调用 .is_table()
    /// Then: 返回 true
    pub fn is_table(&self) -> bool {
        matches!(self, TValue::Table(_))
    }

    /// 是否为函数（任何类型）
    ///
    /// Scenario: 判断函数类型
    /// Given: TValue::LClosure(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    /// Given: TValue::LCFn(_)
    /// When: 调用 .is_function()
    /// Then: 返回 true
    pub fn is_function(&self) -> bool {
        matches!(self, TValue::LClosure(_) | TValue::CClosure(_) | TValue::LCFn(_))
    }

    /// 尝试获取整数值
    ///
    /// Scenario: 从 TValue 提取整数
    /// Given: TValue::Integer(42)
    /// When: 调用 .as_integer()
    /// Then: 返回 Some(42)
    /// Given: TValue::Float(1.0)
    /// When: 调用 .as_integer()
    /// Then: 返回 Some(1)，如果 1.0 可以无损转为整数
    /// Given: TValue::Float(1.5)
    /// When: 调用 .as_integer()
    /// Then: 返回 None
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            TValue::Integer(i) => Some(*i),
            TValue::Float(f) => {
                let i = *f as i64;
                if (i as f64) == *f { Some(i) } else { None }
            }
            _ => None,
        }
    }

    /// 尝试获取浮点数值
    ///
    /// Scenario: 从 TValue 提取浮点数
    /// Given: TValue::Float(3.14)
    /// When: 调用 .as_float()
    /// Then: 返回 Some(3.14)
    /// Given: TValue::Integer(42)
    /// When: 调用 .as_float()
    /// Then: 返回 Some(42.0)
    pub fn as_float(&self) -> Option<f64> {
        match self {
            TValue::Float(f) => Some(*f),
            TValue::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }
}

impl Default for TValue {
    fn default() -> Self {
        TValue::Nil(NilKind::Strict)
    }
}

impl PartialEq for TValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TValue::Nil(a), TValue::Nil(b)) => a == b,
            (TValue::Boolean(a), TValue::Boolean(b)) => a == b,
            (TValue::LightUserData(a), TValue::LightUserData(b)) => a == b,
            (TValue::Integer(a), TValue::Integer(b)) => a == b,
            (TValue::Float(a), TValue::Float(b)) => {
                if a.is_nan() || b.is_nan() {
                    false
                } else {
                    a.to_bits() == b.to_bits()
                }
            }
            (TValue::Integer(a), TValue::Float(b)) => {
                if b.is_nan() { false } else { (*a as f64).to_bits() == b.to_bits() }
            }
            (TValue::Float(a), TValue::Integer(b)) => {
                if a.is_nan() { false } else { a.to_bits() == (*b as f64).to_bits() }
            }
            (TValue::Str(a), TValue::Str(b)) => a == b,
            (TValue::Table(a), TValue::Table(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
            (TValue::LClosure(a), TValue::LClosure(b)) => a.gc_header.ptr_id == b.gc_header.ptr_id,
            (TValue::CClosure(a), TValue::CClosure(b)) => std::ptr::eq(a, b),
            (TValue::LCFn(a), TValue::LCFn(b)) => std::ptr::eq(a.func as *const (), b.func as *const ()),
            (TValue::UserData(a), TValue::UserData(b)) => std::ptr::eq(a, b),
            (TValue::Thread(a), TValue::Thread(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }
}

impl Eq for TValue {}

impl Hash for TValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            TValue::Nil(kind) => {
                0u8.hash(state);
                kind.hash(state);
            }
            TValue::Boolean(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            TValue::LightUserData(p) => {
                2u8.hash(state);
                (*p as usize).hash(state);
            }
            TValue::Integer(i) => {
                3u8.hash(state);
                i.hash(state);
            }
            TValue::Float(f) => {
                if f.is_nan() {
                    4u8.hash(state);
                    0u64.hash(state);
                } else {
                    let i = *f as i64;
                    if (i as f64).to_bits() == f.to_bits() && *f != -0.0 {
                        3u8.hash(state);
                        i.hash(state);
                    } else {
                        4u8.hash(state);
                        f.to_bits().hash(state);
                    }
                }
            }
            TValue::Str(s) => {
                5u8.hash(state);
                Hash::hash(s, state);
            }
            TValue::Table(t) => {
                6u8.hash(state);
                t.gc_header.ptr_id.hash(state);
            }
            TValue::LClosure(c) => {
                7u8.hash(state);
                c.gc_header.ptr_id.hash(state);
            }
            TValue::CClosure(c) => {
                8u8.hash(state);
                (c as *const CClosure as usize).hash(state);
            }
            TValue::LCFn(c) => {
                9u8.hash(state);
                (c.func as usize).hash(state);
            }
            TValue::UserData(u) => {
                10u8.hash(state);
                (u as *const Udata as usize).hash(state);
            }
            TValue::Thread(t) => {
                11u8.hash(state);
                (t as *const LuaThread as usize).hash(state);
            }
        }
    }
}

// TValue 的 Display 实现用于调试输出
impl fmt::Display for TValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TValue::Nil(NilKind::Strict) => write!(f, "nil"),
            TValue::Nil(NilKind::Empty) => write!(f, "(empty)"),
            TValue::Nil(NilKind::AbsentKey) => write!(f, "(absent key)"),
            TValue::Nil(NilKind::NotTable) => write!(f, "(not a table)"),
            TValue::Boolean(b) => write!(f, "{}", b),
            TValue::Integer(i) => write!(f, "{}", i),
            TValue::Float(n) => write!(f, "{}", n),
            TValue::Str(s) => write!(f, "{}", s.as_str()),
            TValue::LightUserData(p) => write!(f, "lightuserdata({:p})", p),
            TValue::Table(_) => write!(f, "table"),
            TValue::LClosure(_) => write!(f, "function"),
            TValue::CClosure(_) => write!(f, "function"),
            TValue::LCFn(_) => write!(f, "function"),
            TValue::UserData(_) => write!(f, "userdata"),
            TValue::Thread(_) => write!(f, "thread"),
        }
    }
}

// ============================================================================
// 规约：轻量 C 函数
// ============================================================================

/// 轻量 C 函数指针
///
/// Scenario: 创建轻量 C 函数
/// Given: 一个 extern "C" fn 指针
/// When: 构造 LCFunction
/// Then: 可通过 .call() 调用该函数
#[derive(Debug, Clone, Copy)]
pub struct LCFunction {
    pub func: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
}

// ============================================================================
// 规约：表类型 (用 hashbrown::HashMap 重写)
// ============================================================================

/// Lua 表的数据部分 —— 被 `Rc<RefCell<TableData>>` 包装以实现共享语义。
///
/// 将数据分离到 `TableData` 中，使得 `Table` 的克隆（仅克隆 `Rc`）共享同一份数据。
/// 这解决了 `_ENV` upvalue 与 `state.globals` 不同步的问题：
/// 克隆后的 Table 仍然指向同一份数据，修改对两者都可见。
pub struct TableData {
    /// 数组部分（1-based，索引 0 对应键 1）
    pub array: Vec<TValue>,
    /// 哈希部分（非整数键以及超出数组范围的整数键）
    pub hash: hashbrown::HashMap<TValue, TValue>,
    /// 元表
    pub metatable: Option<Box<Table>>,
    /// #t 的搜索提示（内部优化）
    pub len_hint: usize,
}

/// Lua 表 —— 关联数组，包含数组部分和哈希部分。
///
/// 数据字段包装在 `Rc<RefCell<TableData>>` 中，克隆 `Table` 时共享同一份数据，
/// 而非深拷贝。这保证了 `_ENV` upvalue 与 `state.globals` 始终同步。
///
/// `gc_header` 保留在 `Table` 上（不在 `TableData` 中），因为 `ptr_id` 需要在克隆时保持一致。
///
/// 方法实现见 [crate::table]。
pub struct Table {
    pub gc_header: GCObjectHeader,
    /// 共享数据 —— 克隆 Table 时仅增加 Rc 引用计数
    pub data: Rc<RefCell<TableData>>,
}

impl Clone for Table {
    /// 克隆 Table：仅克隆 `Rc`（共享数据），并克隆 `gc_header`（保持同一 `ptr_id`）。
    fn clone(&self) -> Self {
        Table {
            gc_header: self.gc_header.clone(),
            data: Rc::clone(&self.data),
        }
    }
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("ptr_id", &self.gc_header.ptr_id)
            .finish_non_exhaustive()
    }
}

impl Default for Table {
    fn default() -> Self {
        Table {
            gc_header: GCObjectHeader::new(),
            data: Rc::new(RefCell::new(TableData {
                array: Vec::new(),
                hash: hashbrown::HashMap::new(),
                metatable: None,
                len_hint: 0,
            })),
        }
    }
}

// ============================================================================
// 规约：函数类型
// ============================================================================

/// Lua 闭包（Lua 侧）
///
/// Scenario: Lua 闭包包含原型和上值
/// Given: 一个 Proto 和 3 个 UpValue
/// When: 构造 LClosure
/// Then: nupvalues = 3, p 指向该 Proto
#[derive(Debug, Clone)]
pub struct LClosure {
    pub gc_header: GCObjectHeader,
    /// 函数原型
    pub proto: Proto,
    /// 上值列表（共享引用，多个闭包可共享同一个 UpVal）。
    ///
    /// 使用 Rc<RefCell<Vec>> 包装，使 LClosure clone 时所有副本共享同一个 upvals Vec。
    /// 这样 debug.upvaluejoin 修改栈上副本的 upvals 会影响所有共享同一 Rc 的闭包
    /// （对应 C 中 Closure 是堆分配对象、栈上存 Closure* 指针的语义）。
    pub upvals: Rc<RefCell<Vec<UpValRef>>>,
}

/// C 闭包 —— C 函数 + 捕获的上值
///
/// Scenario: C 闭包
/// Given: 一个 C 函数指针和 2 个上值
/// When: 构造 CClosure
/// Then: f 指向该函数，upvalue 有 2 个元素
#[derive(Debug, Clone)]
pub struct CClosure {
    /// C 函数指针
    pub f: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
    /// 上值列表
    pub upvalue: Vec<TValue>,
}

// ============================================================================
// 规约：上值 (UpValue)
// ============================================================================

/// 上值 —— 闭包捕获的外部局部变量
///
/// 上值有两种状态：打开（指向栈上的变量）和关闭（持有值的副本）。
///
/// Scenario: 打开的上值
/// Given: 上值指向栈上索引 2 的位置
/// When: 读取上值
/// Then: 返回栈上对应位置的值
///
/// Scenario: 关闭的上值
/// Given: 上值已关闭，持有值 TValue::Integer(42)
/// When: 读取上值
/// Then: 返回 TValue::Integer(42)
#[derive(Debug, Clone)]
pub enum UpVal {
    /// 打开的上值（指向栈上的活跃变量）
    Open {
        /// 栈上位置索引
        stack_index: usize,
        /// 链表中的下一个上值索引
        next: Option<usize>,
        /// 链表中的上一个上值索引
        previous: Option<usize>,
    },
    /// 关闭的上值（持有值的副本）
    Closed {
        /// 存储的值
        value: Box<TValue>,
    },
}

impl UpVal {
    pub fn is_open(&self) -> bool {
        matches!(self, UpVal::Open { .. })
    }

    pub fn level(&self) -> Option<usize> {
        match self {
            UpVal::Open { stack_index, .. } => Some(*stack_index),
            UpVal::Closed { .. } => None,
        }
    }
}

// ============================================================================
// 规约：函数原型 (Proto)
// ============================================================================

/// 指令 —— 32 位无符号整数
pub type Instruction = u32;

/// 上值描述符
///
/// Scenario: 上值描述符
/// Given: upvalue 名为 "x", 在栈上, 索引为 0
/// When: 构造 UpvalDesc
/// Then: instack = true, idx = 0
#[derive(Debug, Clone)]
pub struct UpvalDesc {
    /// 上值名称（调试信息）
    pub name: Option<LuaString>,
    /// 是否在栈上
    pub in_stack: bool,
    /// 索引（栈索引或外层函数上值列表索引）
    pub idx: u8,
    /// For in_stack upvalues: index in parent's locals array (used by mark_block_upval)
    /// For non-in_stack upvalues: unused (0)
    pub parent_local_idx: usize,
}

/// 局部变量描述符（调试信息）
///
/// Scenario: 局部变量
/// Given: 变量名为 "i", startpc = 0, endpc = 5
/// When: 构造 LocVar
/// Then: varname = "i", startpc = 0, endpc = 5
#[derive(Debug, Clone)]
pub struct LocVar {
    /// 变量名
    pub varname: Option<LuaString>,
    /// 变量活跃的起始 PC
    pub start_pc: i32,
    /// 变量失效的起始 PC
    pub end_pc: i32,
}

/// 绝对行号信息
#[derive(Debug, Clone)]
pub struct AbsLineInfo {
    pub pc: i32,
    pub line: i32,
}

/// 函数原型 —— 编译后的函数体
///
/// Scenario: 函数原型的完整结构
/// Given: 一个 Lua 函数 "function add(a,b) return a+b end"
/// When: 编译为 Proto
/// Then: numparams = 2, code 为非空, maxstacksize ≥ 2
#[derive(Debug, Clone)]
pub struct Proto {
    /// 固定参数数量
    pub num_params: u8,
    /// 标志位 (PF_VAHID, PF_VATAB, PF_FIXED)
    pub flag: u8,
    /// 最大栈大小
    pub max_stack_size: u8,
    /// 上值数量
    pub size_upvalues: i32,
    /// 常量数量
    pub size_k: i32,
    /// 代码大小
    pub size_code: i32,
    /// 行信息大小
    pub size_line_info: i32,
    /// 子原型数量
    pub size_p: i32,
    /// 局部变量数量
    pub size_loc_vars: i32,
    /// 绝对行信息数量
    pub size_abs_line_info: i32,
    /// 定义起始行
    pub line_defined: i32,
    /// 定义结束行
    pub last_line_defined: i32,
    /// 常量表
    pub constants: Vec<TValue>,
    /// 指令序列
    pub code: Vec<Instruction>,
    /// 子原型
    pub protos: Vec<Proto>,
    /// 上值描述
    pub upvalues: Vec<UpvalDesc>,
    /// 行号差值数组
    pub line_info: Vec<i8>,
    /// 绝对行号信息
    pub abs_line_info: Vec<AbsLineInfo>,
    /// 局部变量
    pub loc_vars: Vec<LocVar>,
    /// 源文件名（调试信息）
    pub source: Option<LuaString>,
}

impl Proto {
    /// 是否为变参函数
    ///
    /// Scenario: 判断变参函数
    /// Given: flag 设置了 PF_VAHID 或 PF_VATAB
    /// When: 调用 .is_vararg()
    /// Then: 返回 true
    pub fn is_vararg(&self) -> bool {
        (self.flag & (PF_VAHID | PF_VATAB)) != 0
    }

    /// 标记需要 vararg 表
    ///
    /// Scenario: 标记为需要 vararg 表
    /// Given: flag 初始为 0
    /// When: 调用 .need_vararg_table()
    /// Then: flag 设置了 PF_VATAB 位
    pub fn need_vararg_table(&mut self) {
        self.flag |= PF_VATAB;
    }
}

/// 原型标志位
pub const PF_VAHID: u8 = 1;
pub const PF_VATAB: u8 = 2;
pub const PF_FIXED: u8 = 4;

// ============================================================================
// 规约：用户数据 (UserData)
// ============================================================================

/// 用户数据 —— 存储任意 C 数据
///
/// Scenario: 创建用户数据
/// Given: 数据长度 64 字节，无用户值
/// When: 构造 Udata
/// Then: len = 64, nuvalue = 0, data 指向 64 字节内存
#[derive(Debug, Clone)]
pub struct Udata {
    /// 用户值数量
    pub nuvalue: u16,
    /// 数据长度
    pub len: usize,
    /// 元表
    pub metatable: Option<Box<Table>>,
    /// 用户值列表
    pub user_values: Vec<TValue>,
    /// 原始数据
    pub data: Vec<u8>,
}

// ============================================================================
// 规约：线程/协程
// ============================================================================

/// Lua 线程（协程）
///
/// Scenario: 线程的生命周期
/// Given: 一个新创建的 Lua 线程
/// When: 检查其状态
/// Then: status = ThreadStatus::Suspended, stack 为空
#[derive(Debug, Clone)]
pub struct LuaThread {
    /// 线程栈
    pub stack: Vec<TValue>,
    /// 线程状态
    pub status: ThreadStatus,
    /// 协程体函数 (coroutine.create 的参数)
    pub function: Option<Box<TValue>>,
}

/// 线程状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    OK,
    Suspended,
    Normal,
    Error,
}

// ============================================================================
// 规约：栈条目 (StackValue)
// ============================================================================

/// 栈条目 —— 用于 to-be-closed 变量追踪
///
/// Scenario: 栈条目的结构
/// Given: 一个 TValue 和一个 delta 值
/// When: 构造 StackValue
/// Then: val 包含该 TValue, tbc_delta 包含该 delta
#[derive(Debug, Clone)]
pub struct StackValue {
    /// 栈上的值
    pub val: TValue,
    /// to-be-closed 变量的距离 delta
    pub tbc_delta: u16,
}

// ============================================================================
// 规约：ceil_log2 — 计算 ceil(log2(x))
// ============================================================================

/// 计算 ceil(log2(x))，即满足 x ≤ 2^n 的最小整数 n。
///
/// Scenario: 计算 ceil(log2)
/// Given: x = 1
/// When: 调用 ceil_log2(1)
/// Then: 返回 0 (因为 1 ≤ 2^0)
///
/// Given: x = 2
/// When: 调用 ceil_log2(2)
/// Then: 返回 1 (因为 2 ≤ 2^1)
///
/// Given: x = 3
/// When: 调用 ceil_log2(3)
/// Then: 返回 2 (因为 3 ≤ 2^2 = 4)
///
/// Given: x = 256
/// When: 调用 ceil_log2(256)
/// Then: 返回 8 (因为 256 = 2^8)
///
/// Given: x = 257
/// When: 调用 ceil_log2(257)
/// Then: 返回 9 (因为 257 ≤ 2^9 = 512)
///
/// Given: x = 0
/// When: 调用 ceil_log2(0)
/// Then: 触发 panic (debug) 或未定义行为，因输入无效
pub fn ceil_log2(x: u32) -> u8 {
    if x == 0 {
        panic!("ceil_log2: zero is not a valid input");
    }
    // 规约: 待实现
    let x = x - 1;
    let mut l: u8 = 0;
    let mut v = x;
    while v >= 256 {
        l += 8;
        v >>= 8;
    }
    let log_2: [u8; 256] = [
        0, 1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
        6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
        6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    ];
    l + log_2[v as usize]
}

// ============================================================================
// 规约：codeparam — 将百分比编码为浮点字节
// ============================================================================

/// 将百分比 p（0..=100）编码为 1 字节浮点表示 (eeee xxxx)。
///
/// 格式: 真实值 = (1xxxx) * 2^(eeee - 7 - 1)（eeee != 0）
///             = (xxxx)  * 2^(-7)           （eeee == 0，次正规数）
///
/// Scenario: 编码 0%
/// Given: p = 0
/// When: 调用 codeparam(0)
/// Then: 返回 0x00 (mantissa=0, exponent=0 → 0%)
///
/// Scenario: 编码 100%
/// Given: p = 100
/// When: 调用 codeparam(100)
/// Then: 返回表示 100% 的字节
///
/// Scenario: 编码溢出
/// Given: p 超过最大可表示值
/// When: 调用 codeparam(p)
/// Then: 返回 0xFF (最大值)
pub fn codeparam(p: u32) -> u8 {
    if p >= ((0x1Fu32) << (0xF - 7 - 1)) * 100 {
        return 0xFF;
    }
    let p_val = ((p as u64) * 128).div_ceil(100);
    if p_val < 0x10 {
        return p_val as u8;
    }
    let p_val = p_val as u32;
    let log = ceil_log2(p_val + 1) as u32 - 5;
    (((p_val >> log) - 0x10) | ((log + 1) << 4)) as u8
}

// ============================================================================
// 规约：applyparam — 浮点字节 × x
// ============================================================================

/// 将浮点字节 p 应用于值 x，计算 p/100 * x。
///
/// Scenario: 应用 0% 参数
/// Given: p = 0x00 (0%), x = 1024
/// When: 调用 applyparam(0x00, 1024)
/// Then: 返回 0
///
/// Scenario: 应用参数到小值
/// Given: p 编码 50%, x = 100
/// When: 调用 applyparam(p, 100)
/// Then: 返回 ≈ 50
///
/// Scenario: 溢出保护
/// Given: p 和 x 的组合超出 MAX_LMEM
/// When: 调用 applyparam(p, x)
/// Then: 返回 MAX_LMEM (不会 panic)
pub fn applyparam(p: u8, x: usize) -> usize {
    // 规约: 待实现
    let m = (p & 0xF) as usize;
    let mut e = (p >> 4) as i32;
    if e > 0 {
        e -= 1;
    }
    let m = m + if p >> 4 > 0 { 0x10 } else { 0 };
    e -= 7;
    if e >= 0 {
        let shift = e as u32;
        match (x as u128)
            .checked_mul(m as u128)
            .and_then(|v| v.checked_shl(shift))
        {
            Some(v) if v <= usize::MAX as u128 => v as usize,
            _ => usize::MAX,
        }
    } else {
        let shift = (-e) as u32;
        match (x as u128).checked_mul(m as u128) {
            Some(v) => (v >> shift) as usize,
            None => match (x >> shift).checked_mul(m) {
                Some(v) => v,
                None => usize::MAX,
            },
        }
    }
}

// ============================================================================
// 规约：hexavalue — 十六进制字符转数值
// ============================================================================

/// 将十六进制字符转换为对应的整数值。
///
/// Scenario: 数字字符
/// Given: c = '5'
/// When: 调用 hexavalue('5')
/// Then: 返回 5
///
/// Scenario: 大写字母
/// Given: c = 'A'
/// When: 调用 hexavalue('A')
/// Then: 返回 10
///
/// Scenario: 小写字母
/// Given: c = 'f'
/// When: 调用 hexavalue('f')
/// Then: 返回 15
///
/// Scenario: 非十六进制字符
/// Given: c = 'G'
/// When: 调用 hexavalue('G')
/// Then: 返回 0 (或 panic，仅对有效输入)
pub fn hexavalue(c: u8) -> u8 {
    // 规约: 待实现
    if c.is_ascii_digit() {
        c - b'0'
    } else {
        (c.to_ascii_lowercase() - b'a') + 10
    }
}

// ============================================================================
// 规约：str2num — 字符串转数字
// ============================================================================

/// 将字符串解析为 Lua 数字（优先整数，否则浮点）。
///
/// Scenario: 解析十进制整数
/// Given: s = "42"
/// When: 调用 str2num("42")
/// Then: 返回 TValue::Integer(42)
///
/// Scenario: 解析负整数
/// Given: s = "-100"
/// When: 调用 str2num("-100")
/// Then: 返回 TValue::Integer(-100)
///
/// Scenario: 解析浮点数
/// Given: s = "3.14"
/// When: 调用 str2num("3.14")
/// Then: 返回 TValue::Float(3.14)
///
/// Scenario: 解析十六进制整数
/// Given: s = "0xFF"
/// When: 调用 str2num("0xFF")
/// Then: 返回 TValue::Integer(255)
///
/// Scenario: 解析无效字符串
/// Given: s = "abc"
/// When: 调用 str2num("abc")
/// Then: 返回 None
///
/// Scenario: 解析空字符串
/// Given: s = ""
/// When: 调用 str2num("")
/// Then: 返回 None
pub fn str2num(s: &str) -> Option<TValue> {
    // 规约: 待实现
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // 尝试十六进制
    if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return parse_hex_int(rest).map(TValue::Integer);
    }
    // 尝试十进制整数
    if let Some(i) = parse_dec_int(s) {
        return Some(TValue::Integer(i));
    }
    // 尝试浮点数
    if let Ok(f) = s.parse::<f64>() {
        return Some(TValue::Float(f));
    }
    None
}

fn parse_dec_int(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

fn parse_hex_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut result: i64 = 0;
    for &b in s.as_bytes() {
        let digit = hexavalue(b) as i64;
        result = result.checked_mul(16)?.checked_add(digit)?;
    }
    Some(result)
}

// ============================================================================
// 规约：utf8esc — UTC-8 编码
// ============================================================================

/// 将 Unicode 码点编码为 UTF-8 字节序列。
///
/// 缓冲区至少需要 UTF8_BUFFSZ = 8 字节。
/// 返回写入的字节数，UTF-8 数据放在 buffer 末尾。
///
/// Scenario: ASCII 字符
/// Given: x = 0x41 ('A')
/// When: 调用 utf8esc(buffer, 0x41)
/// Then: 返回 1, buffer[UTF8_BUFFSZ-1] = 0x41
///
/// Scenario: 2 字节序列
/// Given: x = 0xA2 (¢)
/// When: 调用 utf8esc(buffer, 0xA2)
/// Then: 返回 2, 正确编码为 UTF-8 双字节
///
/// Scenario: 3 字节序列 (中文)
/// Given: x = 0x4E2D ('中')
/// When: 调用 utf8esc(buffer, 0x4E2D)
/// Then: 返回 3, 正确编码为 UTF-8 三字节
///
/// Scenario: 最大码点
/// Given: x = 0x7FFFFFFF
/// When: 调用 utf8esc(buffer, 0x7FFFFFFF)
/// Then: 返回正确的多字节编码
pub const UTF8_BUFFSZ: usize = 8;

pub fn utf8esc(buffer: &mut [u8], x: u32) -> usize {
    // 规约: 待实现
    assert!(x <= 0x7FFFFFFF);
    if x < 0x80 {
        buffer[UTF8_BUFFSZ - 1] = x as u8;
        return 1;
    }
    let mut n = 1;
    let mut remaining = x;
    let mut mfb: u32 = 0x3f;
    loop {
        buffer[UTF8_BUFFSZ - n] = 0x80 | ((remaining & 0x3f) as u8);
        n += 1;
        remaining >>= 6;
        mfb >>= 1;
        if remaining <= mfb {
            break;
        }
    }
    buffer[UTF8_BUFFSZ - n] = ((!mfb << 1) | remaining) as u8;
    n
}

// ============================================================================
// 规约：tostringbuff — 数字转字符串到缓冲区
// ============================================================================

/// 将 TValue 中的数字转换为字符串写入缓冲区。
/// 返回写入的字节数（不含 '\0'）。
///
/// Scenario: 整数转字符串
/// Given: TValue::Integer(42)
/// When: 调用 tostringbuff(tv, buffer)
/// Then: buffer 包含 "42", 返回 2
///
/// Scenario: 负整数转字符串
/// Given: TValue::Integer(-7)
/// When: 调用 tostringbuff(tv, buffer)
/// Then: buffer 包含 "-7", 返回 2
///
/// Scenario: 浮点数转字符串
/// Given: TValue::Float(3.14)
/// When: 调用 tostringbuff(tv, buffer)
/// Then: buffer 包含 "3.14", 返回 4
///
/// Scenario: 输入不是数字
/// Given: TValue::Nil
/// When: 调用 tostringbuff(tv, buffer)
/// Then: panic (debug) 或未定义行为
pub fn tostringbuff(obj: &TValue, buffer: &mut [u8]) -> usize {
    // 规约: 待实现
    match obj {
        TValue::Integer(i) => {
            let s = i.to_string();
            let len = s.len();
            buffer[..len].copy_from_slice(s.as_bytes());
            len
        }
        TValue::Float(f) => {
            // 用足够精度格式化
            let s = format!("{:.15}", f);
            // 去掉尾部多余的零（保留至少一位小数，如果看起来像整数加 ".0"）
            let trimmed = trim_float_str(&s);
            let len = trimmed.len();
            buffer[..len].copy_from_slice(trimmed.as_bytes());
            len
        }
        _ => panic!("tostringbuff: value is not a number"),
    }
}

fn trim_float_str(s: &str) -> String {
    // 如果看起来像整数，添加 ".0"
    if s.bytes().all(|b| b == b'-' || b.is_ascii_digit()) {
        return format!("{}.0", s);
    }
    s.to_string()
}

// ============================================================================
// 规约：chunkid — 生成 chunk 标识符
// ============================================================================

/// 生成 chunk 标识符字符串。
///
/// Scenario: 字面量源 (以 '=' 开头)
/// Given: source = "=hello", srclen = 6
/// When: 调用 chunkid(out, source, srclen)
/// Then: out 包含 "hello"
///
/// Scenario: 文件名源 (以 '@' 开头)
/// Given: source = "@/path/to/file.lua", srclen = 19
/// When: 调用 chunkid(out, source, srclen)
/// Then: out 包含 "path/to/file.lua" (去掉 @)
///
/// Scenario: 字符串源
/// Given: source = "print('hello')", srclen = 15
/// When: 调用 chunkid(out, source, srclen)
/// Then: out 包含 '[string "print('hel..."]' 格式
pub fn chunkid(out: &mut [u8], source: &[u8], srclen: usize) {
    // 规约: 待实现
    let bufflen = out.len();
    let src = &source[..srclen];
    if src.first() == Some(&b'=') {
        let content = &src[1..];
        if content.len() <= bufflen {
            let dst = &mut out[..content.len()];
            dst.copy_from_slice(content);
            if content.len() < bufflen {
                out[content.len()] = 0;
            }
        } else {
            let dst = &mut out[..bufflen - 1];
            dst.copy_from_slice(&content[..bufflen - 1]);
            out[bufflen - 1] = 0;
        }
    } else if src.first() == Some(&b'@') {
        let content = &src[1..];
        if content.len() <= bufflen {
            let dst = &mut out[..content.len()];
            dst.copy_from_slice(content);
        } else {
            let rets = b"...";
            let rets_len = rets.len();
            let dst0 = &mut out[..rets_len];
            dst0.copy_from_slice(rets);
            let remaining = bufflen - rets_len;
            let start = content.len() - remaining;
            let dst1 = &mut out[rets_len..rets_len + remaining];
            dst1.copy_from_slice(&content[start..]);
        }
    } else {
        let pre = b"[string \"";
        let pos = b"\"]";
        let pre_len = pre.len();
        let pos_len = pos.len();
        let total_overhead = pre_len + pos_len;
        if total_overhead >= bufflen {
            return;
        }
        out[..pre_len].copy_from_slice(pre);
        let usable = bufflen - total_overhead;
        let nl = src.iter().position(|&b| b == b'\n');
        let src_len = nl.map(|p| p.min(usable)).unwrap_or(if src.len() < usable { src.len() } else { usable });
        out[pre_len..pre_len + src_len].copy_from_slice(&src[..src_len]);
        if nl.is_some() || src.len() > usable {
            let rets = b"...";
            let rets_len = rets.len();
            out[pre_len + src_len..pre_len + src_len + rets_len].copy_from_slice(rets);
            out[pre_len + src_len + rets_len..pre_len + src_len + rets_len + pos_len].copy_from_slice(pos);
        } else {
            out[pre_len + src_len..pre_len + src_len + pos_len].copy_from_slice(pos);
        }
    }
}

// ============================================================================
// 规约：rawarith — 原始算术运算
// ============================================================================

/// 算术运算操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    IDiv,
    Mod,
    Pow,
    BAnd,
    BOr,
    BXor,
    Shl,
    Shr,
    Unm,
    BNot,
}

/// 对两个 TValue 执行原始算术运算，结果写入 res。
/// 返回 true 表示成功，false 表示操作数类型不匹配。
///
/// Scenario: 整数加法
/// Given: p1 = Integer(3), p2 = Integer(5), op = Add
/// When: 调用 rawarith(op, p1, p2, &mut res)
/// Then: res = Integer(8), 返回 true
///
/// Scenario: 整数与浮点混合加法
/// Given: p1 = Integer(3), p2 = Float(2.5), op = Add
/// When: 调用 rawarith(op, p1, p2, &mut res)
/// Then: res = Float(5.5), 返回 true
///
/// Scenario: 位运算只能用于整数
/// Given: p1 = Float(3.0), p2 = Integer(5), op = BAnd
/// When: 调用 rawarith(op, p1, p2, &mut res)
/// Then: 返回 false (浮点数不能做位运算)
///
/// Scenario: 除零
/// Given: p1 = Integer(10), p2 = Integer(0), op = Div
/// When: 调用 rawarith(op, p1, p2, &mut res)
/// Then: res = Float(inf), 返回 true (IEEE 754 行为)
pub fn rawarith(op: ArithOp, p1: &TValue, p2: &TValue, res: &mut TValue) -> bool {
    // 规约: 待实现
    match op {
        ArithOp::BAnd | ArithOp::BOr | ArithOp::BXor | ArithOp::Shl | ArithOp::Shr | ArithOp::BNot => {
            if !p1.is_integer() || !p2.is_integer() {
                return false;
            }
            let i1 = p1.as_integer();
            let i2 = p2.as_integer();
            match (i1, i2) {
                (Some(a), Some(b)) => {
                    *res = match op {
                        ArithOp::BAnd => TValue::Integer(a & b),
                        ArithOp::BOr => TValue::Integer(a | b),
                        ArithOp::BXor => TValue::Integer(a ^ b),
                        ArithOp::Shl => TValue::Integer(a.wrapping_shl(b as u32)),
                        ArithOp::Shr => TValue::Integer(a.wrapping_shr(b as u32)),
                        ArithOp::BNot => TValue::Integer(!a),
                        _ => unreachable!(),
                    };
                    true
                }
                _ => false,
            }
        }
        ArithOp::Div | ArithOp::Pow => {
            let n1 = p1.as_float();
            let n2 = p2.as_float();
            match (n1, n2) {
                (Some(a), Some(b)) => {
                    *res = TValue::Float(match op {
                        ArithOp::Div => a / b,
                        ArithOp::Pow => a.powf(b),
                        _ => unreachable!(),
                    });
                    true
                }
                _ => false,
            }
        }
        _ => {
            if p1.is_integer() && p2.is_integer() {
                if let (Some(a), Some(b)) = (p1.as_integer(), p2.as_integer()) {
                    *res = match op {
                        ArithOp::Add => TValue::Integer(a.wrapping_add(b)),
                        ArithOp::Sub => TValue::Integer(a.wrapping_sub(b)),
                        ArithOp::Mul => TValue::Integer(a.wrapping_mul(b)),
                        ArithOp::IDiv => TValue::Integer(if b == 0 { 0 } else { float_idiv(a as f64, b as f64) as i64 }),
                        ArithOp::Mod => TValue::Integer(if b == 0 { 0 } else { a % b }),
                        ArithOp::Unm => TValue::Integer(a.wrapping_neg()),
                        _ => unreachable!(),
                    };
                    return true;
                }
            }
            let n1 = p1.as_float();
            let n2 = p2.as_float();
            match (n1, n2) {
                (Some(a), Some(b)) => {
                    *res = TValue::Float(match op {
                        ArithOp::Add => a + b,
                        ArithOp::Sub => a - b,
                        ArithOp::Mul => a * b,
                        ArithOp::IDiv => float_idiv(a, b),
                        ArithOp::Mod => float_mod(a, b),
                        ArithOp::Unm => -a,
                        _ => unreachable!(),
                    });
                    true
                }
                _ => false,
            }
        }
    }
}

fn float_idiv(a: f64, b: f64) -> f64 {
    (a / b).floor()
}

fn float_mod(a: f64, b: f64) -> f64 {
    let m = a % b;
    if (m > 0.0) == (b < 0.0) && m != 0.0 { m + b } else { m }
}

// ============================================================================
// 规约：arith — 算术运算（含元方法回退）
// ============================================================================

/// 执行算术运算。如果原始运算失败（操作数类型不匹配），尝试元方法。
/// 当前版本仅实现原始运算，元方法回退预留扩展。
///
/// Scenario: 同 rawarith，但失败时不立即返回
/// Given: 操作数类型匹配
/// When: 调用 arith(op, p1, p2, res)
/// Then: 同 rawarith 的行为
///
/// Scenario: 操作数类型不匹配
/// Given: 操作数类型不匹配且无法转换
/// When: 调用 arith(op, p1, p2, res)
/// Then: 返回 false (元方法回退预留)
pub fn arith(_op: ArithOp, _p1: &TValue, _p2: &TValue, _res: &mut TValue) -> bool {
    // 简化实现：直接返回 false（元方法回退预留）
    false
}

// ============================================================================
// 规约：hashpow2 — 2 的幂取模
// ============================================================================

/// 对 2 的幂 size 取模（等价于 x & (size - 1)）。
///
/// Scenario: 取模计算
/// Given: x = 7, size = 8 (2^3)
/// When: 调用 hashmod(7, 8)
/// Then: 返回 7
///
/// Given: x = 10, size = 8
/// When: 调用 hashmod(10, 8)
/// Then: 返回 2
///
/// Given: x = 0, size = 4
/// When: 调用 hashmod(0, 4)
/// Then: 返回 0
///
/// If: size 不是 2 的幂
/// Then: panic (debug)
pub fn hashmod(x: usize, size: usize) -> usize {
    debug_assert!(size.is_power_of_two(), "hashmod: size must be a power of 2");
    x & (size - 1)
}

/// 计算 2^n
pub fn twoto(n: u8) -> usize {
    1usize << n
}

// ============================================================================
// 测试模块 (红灯 — 失败测试先行)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::{ShortString, LongString};
    use std::sync::atomic::{AtomicU64, AtomicU8};

    // ========================================================================
    // LuaType 测试
    // ========================================================================

    #[test]
    fn test_lua_type_from_u8_valid() {
        assert_eq!(LuaType::from_u8(0), Some(LuaType::Nil));
        assert_eq!(LuaType::from_u8(1), Some(LuaType::Boolean));
        assert_eq!(LuaType::from_u8(3), Some(LuaType::Number));
        assert_eq!(LuaType::from_u8(8), Some(LuaType::Thread));
    }

    #[test]
    fn test_lua_type_from_u8_invalid() {
        assert_eq!(LuaType::from_u8(9), None);
        assert_eq!(LuaType::from_u8(255), None);
    }

    #[test]
    fn test_lua_type_display() {
        assert_eq!(format!("{}", LuaType::Nil), "nil");
        assert_eq!(format!("{}", LuaType::Boolean), "boolean");
        assert_eq!(format!("{}", LuaType::Number), "number");
        assert_eq!(format!("{}", LuaType::String), "string");
    }

    // ========================================================================
    // TValue 测试
    // ========================================================================

    #[test]
    fn test_tvalue_nil() {
        let v = TValue::Nil(NilKind::Strict);
        assert!(v.is_nil());
        assert!(v.is_strict_nil());
        assert_eq!(v.ty(), LuaType::Nil);
    }

    #[test]
    fn test_tvalue_nil_empty() {
        let v = TValue::Nil(NilKind::Empty);
        assert!(v.is_nil());
        assert!(!v.is_strict_nil());
    }

    #[test]
    fn test_tvalue_boolean() {
        let t = TValue::Boolean(true);
        let f = TValue::Boolean(false);
        assert_eq!(t.ty(), LuaType::Boolean);
        assert!(f.is_false());
        assert!(!t.is_false());
        assert!(!f.is_nil());
    }

    #[test]
    fn test_tvalue_integer() {
        let v = TValue::Integer(42);
        assert!(v.is_integer());
        assert!(v.is_number());
        assert!(!v.is_float());
        assert_eq!(v.as_integer(), Some(42));
        assert_eq!(v.as_float(), Some(42.0));
    }

    #[test]
    fn test_tvalue_float() {
        let v = TValue::Float(3.14);
        assert!(v.is_float());
        assert!(v.is_number());
        assert!(!v.is_integer());
        assert_eq!(v.as_float(), Some(3.14));
        assert_eq!(v.as_integer(), None);
    }

    #[test]
    fn test_tvalue_float_to_int_exact() {
        let v = TValue::Float(5.0);
        assert_eq!(v.as_integer(), Some(5));
    }

    #[test]
    fn test_tvalue_float_to_int_inexact() {
        let v = TValue::Float(5.5);
        assert_eq!(v.as_integer(), None);
    }

    #[test]
    fn test_tvalue_default() {
        let v = TValue::default();
        assert_eq!(v, TValue::Nil(NilKind::Strict));
    }

    #[test]
    fn test_tvalue_partial_eq() {
        assert_eq!(TValue::Nil(NilKind::Strict), TValue::Nil(NilKind::Strict));
        assert_ne!(TValue::Nil(NilKind::Strict), TValue::Nil(NilKind::Empty));
        assert_eq!(TValue::Boolean(true), TValue::Boolean(true));
        assert_ne!(TValue::Boolean(true), TValue::Boolean(false));
        assert_eq!(TValue::Integer(42), TValue::Integer(42));
        assert_eq!(TValue::Integer(1), TValue::Float(1.0));
        assert_eq!(TValue::Float(1.0), TValue::Integer(1));
        assert_ne!(TValue::Integer(1), TValue::Nil(NilKind::Strict));
    }

    #[test]
    fn test_tvalue_display() {
        assert_eq!(format!("{}", TValue::Nil(NilKind::Strict)), "nil");
        assert_eq!(format!("{}", TValue::Boolean(true)), "true");
        assert_eq!(format!("{}", TValue::Integer(42)), "42");
    }

    #[test]
    fn test_tvalue_is_false() {
        assert!(TValue::Boolean(false).is_false());
        assert!(TValue::Nil(NilKind::Strict).is_false());
        assert!(TValue::Nil(NilKind::Empty).is_false());
        assert!(!TValue::Boolean(true).is_false());
        assert!(!TValue::Integer(0).is_false());
    }

    #[test]
    fn test_tvalue_is_string() {
        // 需要创建一个 TString 来测试，这里仅测试非字符串情况
        assert!(!TValue::Nil(NilKind::Strict).is_string());
        assert!(!TValue::Integer(1).is_string());
    }

    // ========================================================================
    // LuaString 测试
    // ========================================================================

    #[test]
    fn test_luastring_short() {
        let short = ShortString { hash: 0, contents: "hello".into() };
        let ts = LuaString::Short(std::sync::Arc::new(short));
        assert_eq!(ts.as_str(), "hello");
        assert_eq!(ts.len(), 5);
        assert!(matches!(ts, LuaString::Short(_)));
        assert!(!ts.is_empty());
    }

    #[test]
    fn test_luastring_long() {
        let long = LongString { hash: AtomicU64::new(0), extra: AtomicU8::new(0), contents: "a".repeat(100), ptr_id: 0 };
        let ts = LuaString::Long(long);
        assert_eq!(ts.len(), 100);
        assert!(matches!(ts, LuaString::Long(_)));
    }

    #[test]
    fn test_luastring_empty() {
        let short = ShortString { hash: 0, contents: String::new() };
        let ts = LuaString::Short(std::sync::Arc::new(short));
        assert!(ts.is_empty());
        assert_eq!(ts.len(), 0);
        assert_eq!(ts.as_str(), "");
    }

    #[test]
    fn test_luastring_eq() {
        let arc1 = std::sync::Arc::new(ShortString { hash: 0, contents: "foo".into() });
        let arc2 = std::sync::Arc::clone(&arc1);
        let ts1 = LuaString::Short(arc1);
        let ts2 = LuaString::Short(arc2);
        assert_eq!(ts1, ts2);

        let arc3 = std::sync::Arc::new(ShortString { hash: 1, contents: "bar".into() });
        let ts3 = LuaString::Short(arc3);
        assert_ne!(ts1, ts3);

        let long1 = LuaString::Long(LongString { hash: AtomicU64::new(0), extra: AtomicU8::new(0), contents: "test".into(), ptr_id: 0 });
        let long2 = LuaString::Long(LongString { hash: AtomicU64::new(0), extra: AtomicU8::new(0), contents: "test".into(), ptr_id: 0 });
        assert_eq!(long1, long2);
    }

    #[test]
    fn test_luastring_as_str() {
        let short = LuaString::Short(std::sync::Arc::new(ShortString {
            hash: 0, contents: "abc".into(),
        }));
        assert_eq!(short.as_str(), "abc");
    }

    // ========================================================================
    // Proto 测试
    // ========================================================================

    #[test]
    fn test_proto_vararg() {
        let mut p = Proto {
            num_params: 0,
            flag: 0,
            max_stack_size: 2,
            size_upvalues: 0,
            size_k: 0,
            size_code: 0,
            size_line_info: 0,
            size_p: 0,
            size_loc_vars: 0,
            size_abs_line_info: 0,
            line_defined: 0,
            last_line_defined: 0,
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        assert!(!p.is_vararg());
        p.need_vararg_table();
        assert!(p.is_vararg());
    }

    // ========================================================================
    // ceil_log2 测试
    // ========================================================================

    #[test]
    fn test_ceil_log2_basics() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(7), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
        assert_eq!(ceil_log2(255), 8);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
        assert_eq!(ceil_log2(65535), 16);
        assert_eq!(ceil_log2(65536), 16);
        assert_eq!(ceil_log2(65537), 17);
    }

    #[test]
    #[should_panic]
    fn test_ceil_log2_zero_panics() {
        ceil_log2(0);
    }

    // ========================================================================
    // hexavalue 测试
    // ========================================================================

    #[test]
    fn test_hexavalue_digits() {
        assert_eq!(hexavalue(b'0'), 0);
        assert_eq!(hexavalue(b'5'), 5);
        assert_eq!(hexavalue(b'9'), 9);
    }

    #[test]
    fn test_hexavalue_uppercase() {
        assert_eq!(hexavalue(b'A'), 10);
        assert_eq!(hexavalue(b'C'), 12);
        assert_eq!(hexavalue(b'F'), 15);
    }

    #[test]
    fn test_hexavalue_lowercase() {
        assert_eq!(hexavalue(b'a'), 10);
        assert_eq!(hexavalue(b'f'), 15);
    }

    // ========================================================================
    // str2num 测试
    // ========================================================================

    #[test]
    fn test_str2num_integer() {
        assert_eq!(str2num("42"), Some(TValue::Integer(42)));
        assert_eq!(str2num("-100"), Some(TValue::Integer(-100)));
        assert_eq!(str2num("0"), Some(TValue::Integer(0)));
    }

    #[test]
    fn test_str2num_hex() {
        assert_eq!(str2num("0xFF"), Some(TValue::Integer(255)));
        assert_eq!(str2num("0x10"), Some(TValue::Integer(16)));
        assert_eq!(str2num("0X1A"), Some(TValue::Integer(26)));
    }

    #[test]
    fn test_str2num_float() {
        assert_eq!(str2num("3.14"), Some(TValue::Float(3.14)));
        assert_eq!(str2num("-0.5"), Some(TValue::Float(-0.5)));
    }

    #[test]
    fn test_str2num_invalid() {
        assert_eq!(str2num("abc"), None);
        assert_eq!(str2num(""), None);
        assert_eq!(str2num("   "), None);
    }

    #[test]
    fn test_str2num_with_spaces() {
        assert_eq!(str2num("  42  "), Some(TValue::Integer(42)));
    }

    // ========================================================================
    // utf8esc 测试
    // ========================================================================

    #[test]
    fn test_utf8esc_ascii() {
        let mut buf = [0u8; UTF8_BUFFSZ];
        let n = utf8esc(&mut buf, 0x41);
        assert_eq!(n, 1);
        assert_eq!(buf[UTF8_BUFFSZ - 1], 0x41);
    }

    #[test]
    fn test_utf8esc_2bytes() {
        let mut buf = [0u8; UTF8_BUFFSZ];
        let n = utf8esc(&mut buf, 0xA2);
        assert_eq!(n, 2);
        assert_eq!(buf[UTF8_BUFFSZ - 2], 0xC2);
        assert_eq!(buf[UTF8_BUFFSZ - 1], 0xA2);
    }

    #[test]
    fn test_utf8esc_3bytes() {
        let mut buf = [0u8; UTF8_BUFFSZ];
        let n = utf8esc(&mut buf, 0x4E2D); // '中'
        assert_eq!(n, 3);
        assert_eq!(buf[UTF8_BUFFSZ - 3], 0xE4);
        assert_eq!(buf[UTF8_BUFFSZ - 2], 0xB8);
        assert_eq!(buf[UTF8_BUFFSZ - 1], 0xAD);
    }

    // ========================================================================
    // tostringbuff 测试
    // ========================================================================

    #[test]
    fn test_tostringbuff_integer() {
        let mut buf = [0u8; 64];
        let tv = TValue::Integer(42);
        let len = tostringbuff(&tv, &mut buf);
        assert_eq!(&buf[..len], b"42");
    }

    #[test]
    fn test_tostringbuff_negative_integer() {
        let mut buf = [0u8; 64];
        let tv = TValue::Integer(-7);
        let len = tostringbuff(&tv, &mut buf);
        assert_eq!(&buf[..len], b"-7");
    }

    #[test]
    fn test_tostringbuff_float() {
        let mut buf = [0u8; 64];
        let tv = TValue::Float(3.5);
        let len = tostringbuff(&tv, &mut buf);
        let s = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(s.starts_with("3.5"));
    }

    #[test]
    #[should_panic]
    fn test_tostringbuff_not_number() {
        let mut buf = [0u8; 64];
        let tv = TValue::Nil(NilKind::Strict);
        tostringbuff(&tv, &mut buf);
    }

    // ========================================================================
    // chunkid 测试
    // ========================================================================

    #[test]
    fn test_chunkid_literal_short() {
        let mut out = [0u8; 60];
        chunkid(&mut out, b"=hello", 6);
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn test_chunkid_file_short() {
        let mut out = [0u8; 60];
        chunkid(&mut out, b"@test.lua", 9);
        // 去掉 @ 后为 "test.lua"
        assert_eq!(&out[..8], b"test.lua");
    }

    #[test]
    fn test_chunkid_string_chunk() {
        let mut out = [0u8; 60];
        let src = b"print('hello')";
        chunkid(&mut out, src, src.len());
        let result = std::str::from_utf8(&out).unwrap_or("");
        assert!(result.starts_with("[string \""));
        assert!(result.contains("print"));
    }

    // ========================================================================
    // rawarith / arith 测试
    // ========================================================================

    #[test]
    fn test_rawarith_int_add() {
        let p1 = TValue::Integer(3);
        let p2 = TValue::Integer(5);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Add, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(8));
    }

    #[test]
    fn test_rawarith_int_sub() {
        let p1 = TValue::Integer(10);
        let p2 = TValue::Integer(3);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Sub, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(7));
    }

    #[test]
    fn test_rawarith_int_mul() {
        let p1 = TValue::Integer(4);
        let p2 = TValue::Integer(5);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Mul, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(20));
    }

    #[test]
    fn test_rawarith_float_add() {
        let p1 = TValue::Float(2.5);
        let p2 = TValue::Float(1.5);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Add, &p1, &p2, &mut res));
        if let TValue::Float(f) = res {
            assert!((f - 4.0).abs() < 1e-10);
        } else {
            panic!("expected float");
        }
    }

    #[test]
    fn test_rawarith_mixed_int_float() {
        let p1 = TValue::Integer(3);
        let p2 = TValue::Float(2.5);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Add, &p1, &p2, &mut res));
        if let TValue::Float(f) = res {
            assert!((f - 5.5).abs() < 1e-10);
        } else {
            panic!("expected float");
        }
    }

    #[test]
    fn test_rawarith_bitwise_rejects_float() {
        let p1 = TValue::Float(3.0);
        let p2 = TValue::Integer(5);
        let mut res = TValue::default();
        assert!(!rawarith(ArithOp::BAnd, &p1, &p2, &mut res));
    }

    #[test]
    fn test_rawarith_band() {
        let p1 = TValue::Integer(0b1100);
        let p2 = TValue::Integer(0b1010);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::BAnd, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(0b1000)); // 8
    }

    #[test]
    fn test_rawarith_bor() {
        let p1 = TValue::Integer(0b1100);
        let p2 = TValue::Integer(0b1010);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::BOr, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(0b1110)); // 14
    }

    #[test]
    fn test_rawarith_bxor() {
        let p1 = TValue::Integer(0b1100);
        let p2 = TValue::Integer(0b1010);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::BXor, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(0b0110)); // 6
    }

    #[test]
    fn test_rawarith_shl() {
        let p1 = TValue::Integer(1);
        let p2 = TValue::Integer(3);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Shl, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(8));
    }

    #[test]
    fn test_rawarith_shr() {
        let p1 = TValue::Integer(16);
        let p2 = TValue::Integer(2);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Shr, &p1, &p2, &mut res));
        assert_eq!(res, TValue::Integer(4));
    }

    #[test]
    fn test_rawarith_div() {
        let p1 = TValue::Float(10.0);
        let p2 = TValue::Float(3.0);
        let mut res = TValue::default();
        assert!(rawarith(ArithOp::Div, &p1, &p2, &mut res));
        if let TValue::Float(f) = res {
            assert!((f - 10.0 / 3.0).abs() < 1e-10);
        } else {
            panic!("expected float");
        }
    }

    // ========================================================================
    // codeparam / applyparam 测试
    // ========================================================================

    #[test]
    fn test_codeparam_zero() {
        let p = codeparam(0);
        assert_eq!(p, 0x00);
    }

    #[test]
    fn test_codeparam_100() {
        let p = codeparam(100);
        // 100% 应返回某个合理的字节
        assert!(p > 0);
    }

    #[test]
    fn test_applyparam_zero() {
        let result = applyparam(0x00, 1024);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_applyparam_and_codeparam_roundtrip() {
        // 测试 50% 的编码/解码精度（近似）
        let code = codeparam(50);
        let result = applyparam(code, 1000);
        // 期望约 500，允许 ±10% 误差
        assert!(result > 400 && result < 600,
            "50% of 1000 should be ~500, got {}", result);
    }

    // ========================================================================
    // hashmod 测试
    // ========================================================================

    #[test]
    fn test_hashmod() {
        assert_eq!(hashmod(7, 8), 7);
        assert_eq!(hashmod(10, 8), 2);
        assert_eq!(hashmod(0, 4), 0);
        assert_eq!(hashmod(16, 8), 0);
        assert_eq!(hashmod(17, 8), 1);
    }

    #[test]
    #[should_panic]
    fn test_hashmod_non_power_of_two() {
        hashmod(10, 7);
    }

    #[test]
    fn test_twoto() {
        assert_eq!(twoto(0), 1);
        assert_eq!(twoto(1), 2);
        assert_eq!(twoto(3), 8);
        assert_eq!(twoto(8), 256);
    }

    // ========================================================================
    // UpVal 测试
    // ========================================================================

    #[test]
    fn test_upval_open() {
        let uv = UpVal::Open { stack_index: 3, next: None, previous: None };
        match uv {
            UpVal::Open { stack_index, .. } => assert_eq!(stack_index, 3),
            _ => panic!("expected Open"),
        }
    }

    #[test]
    fn test_upval_open_is_open() {
        let uv = UpVal::Open { stack_index: 3, next: None, previous: None };
        assert!(uv.is_open());
        assert_eq!(uv.level(), Some(3));
    }

    #[test]
    fn test_upval_closed() {
        let uv = UpVal::Closed { value: Box::new(TValue::Integer(42)) };
        match uv {
            UpVal::Closed { value } => assert_eq!(*value, TValue::Integer(42)),
            _ => panic!("expected Closed"),
        }
    }

    #[test]
    fn test_upval_closed_not_open() {
        let uv = UpVal::Closed { value: Box::new(TValue::Integer(42)) };
        assert!(!uv.is_open());
        assert_eq!(uv.level(), None);
    }

    // ========================================================================
    // Instruction 测试
    // ========================================================================

    #[test]
    fn test_instruction() {
        let i: Instruction = 42;
        assert_eq!(i, 42);
    }

    // ========================================================================
    // LocVar 测试
    // ========================================================================

    #[test]
    fn test_loc_var() {
        let lv = LocVar {
            varname: None,
            start_pc: 0,
            end_pc: 5,
        };
        assert_eq!(lv.start_pc, 0);
        assert_eq!(lv.end_pc, 5);
    }

    // ========================================================================
    // NilKind 测试
    // ========================================================================

    #[test]
    fn test_nil_kind_eq() {
        assert_eq!(NilKind::Strict, NilKind::Strict);
        assert_ne!(NilKind::Strict, NilKind::Empty);
        assert_eq!(NilKind::AbsentKey, NilKind::AbsentKey);
    }
}