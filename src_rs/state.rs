use crate::alloc::MyRc;
use crate::debug::runerror;
use crate::execute::{VmError, VmExecutor, VmResult};
use crate::gc::{GCObjectHeader, GCState};
#[cfg(lua_use_longjmp)]
use crate::helper::JmpBuf;
use crate::mem::SlotAlloc;
use crate::mem::{AllocOwner, Allocator, DefaultAllocator, MemState};
use crate::objects::{
    BuiltinFn, BuiltinFnPtr, Instruction, LClosure, LuaThread, LuaType, NilKind, Proto, TValue,
    ThreadContext, UpVal, UpValRef, UpValVec, WeakValue,
};
use crate::strings::{lua_string_as_str, lua_string_len, StringTable};
use crate::table::Table;
use crate::tm::DefaultMetatables;
use crate::tm::{init_tmnames, TM_N};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::ptr::NonNull;
use std::rc::{Rc, Weak};

// ============================================================================
// GC 相关代码已全部迁移至 gc.rs
// - GCState、三色标记、sweep、弱表清理、finalizer、GC 统计、时间统计
// - impl LuaState 的 GC 入口方法（collect_gc / maybe_collect_gc / step_gc /
//   concat_gc_check / collect_gc_inner / mark_tvalue / process_ephemerons /
//   collect_finalizers / call_finalizers / collect_roots_into / collect_thread_roots）
// - impl GlobalState 的 GC 辅助方法（clear_weak_tables / register_weak_table /
//   register_finobj / register_ud_finobj / mode_key_cached / gc_key_cached）
// 通过 crate::gc 模块导出使用。
// ============================================================================

pub const LUA_YIELD: i32 = 1;
pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const ERR_FILE: i32 = 6; // LUA_ERRERR + 1, used by luaL_loadfilex
pub const MULT_RET: i32 = -1;

const LUA_SIGNATURE: &[u8] = b"\x1bLua";

pub const LUA_MINSTACK: usize = 20;
pub const BASIC_STACK_SIZE: usize = 256;
pub const EXTRA_STACK: usize = 64;

pub const MIN_STACK: usize = LUA_MINSTACK;
pub const LUAI_MAXSTACK: usize = 1000000;
pub const LUAI_MAXCCALLS: u32 = 200;
pub const STACKERRSPACE: usize = 200;

/// __call 元方法链的最大深度 (对应 C 的 MAX_CCMT = 0xf << 8, 4 位计数器, 上限 15)
///
/// 对应 src/lstate.h 中的 MAX_CCMT 定义。当 __call 链超过 15 层时报
/// "'__call' chain too long" 错误。
pub const MAX_CALL_CHAIN: usize = 15;
pub const MAXSTACK_BYSIZET: usize = (usize::MAX / std::mem::size_of::<TValue>()) - STACKERRSPACE;
pub const MAXSTACK: usize = if LUAI_MAXSTACK < MAXSTACK_BYSIZET {
    LUAI_MAXSTACK
} else {
    MAXSTACK_BYSIZET
};
pub const ERRORSTACKSIZE: usize = MAXSTACK + STACKERRSPACE;

/// pcall 类型 — 区分 pcall 和 xpcall 的 error 处理行为
#[derive(Clone, Debug)]
pub enum PcallKind<'a> {
    /// pcall: error 时返回 (false, error)
    Pcall,
    /// xpcall: error 时调用 handler(error)，返回 (false, handler_result)
    Xpcall { handler: TValue<'a> },
}

/// pcall 保护状态 — 对应 C Lua 的 CIST_YPCALL CallInfo
///
/// call_pcall/call_xpcall 在调用 state.pcall 前 push 此结构。
/// yield 穿过 pcall 后，pcall 的 C 函数栈帧被销毁，但保护状态保留在栈中。
/// 当 inner_func 后续执行 error 或正常返回时，由 execute_loop 检查并处理
/// （对应 C Lua 的 precover + finishpcallk 机制）。
#[derive(Clone, Debug)]
pub struct PcallProtection<'a> {
    /// pcall 调用者的执行上下文（saved_*）— 用于恢复 pcall 调用者的执行
    pub saved_code: Rc<Vec<Instruction>>,
    pub saved_constants: Rc<Vec<TValue<'a>>>,
    /// 调用者的子原型列表 — Rc 共享，yield 穿越 pcall 时 O(1) 引用计数
    pub saved_protos: Rc<Vec<Rc<Proto<'a>>>>,
    pub saved_base: usize,
    pub saved_pc: usize,
    pub saved_num_params: u8,
    pub saved_is_vararg: bool,
    pub saved_proto_flag: u8,
    pub saved_nextraargs: i32,
    pub saved_closure_upvals: Rc<RefCell<UpValVec<'a>>>,
    pub saved_tbc_list: Option<usize>,
    /// pcall 的 func 位置（栈索引）— 用于截断栈和放置返回值
    pub func_idx: usize,
    /// pcall 期望的返回值数量（-1 = MULTRET）
    pub nresults: i32,
    /// pcall 类型 — 区分 pcall 和 xpcall 的 error/return 处理
    pub pcall_kind: PcallKind<'a>,
    /// saved_* 是否已填充（即是否被 yield 穿过）
    /// false: call_pcall/call_xpcall push 的空壳，由 state.pcall 处理 error
    /// true: yield 穿过后由 state.pcall 更新，由 execute_loop 处理 error/return
    pub saved_filled: bool,
    /// 是否为元方法调用 (call_tm_res 推入)
    /// yield 穿过元方法后，resume 时元方法返回，由 op_return 检查并执行
    /// continuation（对应 C Lua 的 luaV_finishOp 机制）
    pub is_metamethod: bool,
    /// 元方法结果的栈索引 (res 参数) — continuation 时将结果放入此槽位
    pub metamethod_res: usize,
    /// 元方法调用前的 call_stack 长度 — 用于区分元方法自身返回 vs 元方法调用的函数返回
    /// 当 call_stack.len() == saved_call_stack_len 时，是元方法自身返回
    pub saved_call_stack_len: usize,
    /// 是否为 __close continuation (call_close_method 推入)
    /// yield 穿过 __close 后，resume 时 __close 返回，由 op_return 检查并执行
    /// continuation（对应 C Lua 的 luaV_finishOp 对 OP_RETURN/OP_CLOSE 的 savedpc-- 机制）
    pub is_close_continuation: bool,
    /// 是否为 pairs continuation (call_pairs 调用 __pairs 时推入)
    /// yield 穿过 __pairs 后，resume 时 __pairs 返回，由 op_return 检查并执行
    /// continuation（对应 C Lua 的 lua_callk + pairscont 机制）
    /// 与普通 pcall continuation 区别: 不 push true 前缀，直接返回 __pairs 的结果
    pub is_pairs_continuation: bool,
    /// yield 穿越 pcall 时保存的外层 call_stack 帧 — 由 finish_pcall_return 恢复
    /// 这些帧持有 Rc 引用（code/constants/protos 等），丢弃会导致悬空指针。
    /// 不合并到 call_stack: 否则 resume 后内层帧 pop 完会错误 pop 外层帧，
    /// 导致 state.exec.base 跳过 pcall 的 func_idx+1。
    pub saved_call_stack: Vec<crate::objects::CallFrame<'a>>,
}

pub struct GlobalShared<'a> {
    pub gcstopem: bool,
    pub gc: GCState<'a>,
    pub memerrmsg: TValue<'a>,
    /// 分配器宿主 —— 本块内所有经槽分配的对象（`gc` 存储里的弱引用、`memerrmsg`）
    /// 析构时都要经它释放。**声明在最后** ⇒ 宿主晚于 `gc`/`memerrmsg` 析构；
    /// 与 `GlobalState::mem.allocator` 共享同一宿主（同一 `Rc`），故共享块句柄
    /// 即便比 `GlobalState` 活得久也不会出现悬垂槽（见 [`AllocOwner`]）。
    alloc: AllocOwner,
}

impl<'a> GlobalShared<'a> {
    /// 新建共享全局块：自持分配器宿主，`memerrmsg` 与后续注册的 GC 对象共用其槽。
    ///
    /// `owner` 由调用方（`GlobalState::new_shared` / 测试 harness）建立并**同时**
    /// 交给 `GlobalState::mem`，两处共享同一宿主。
    pub fn new(owner: AllocOwner, string_table: &StringTable) -> Self {
        let slot = SlotAlloc::from_owner(&owner);
        Self {
            gcstopem: false,
            gc: GCState::default_incremental(),
            memerrmsg: crate::strings::new_lstr(slot, string_table, crate::strings::MEMERRMSG),
            alloc: owner,
        }
    }

    /// 本块的分配器槽（构造期在拿到共享块句柄前分配对象用）。
    ///
    /// 槽有效性由 `alloc` 字段保证：它在 `gc` 之后析构，故块内对象释放时宿主必然存活。
    #[inline]
    pub fn alloc_slot(&self) -> SlotAlloc {
        SlotAlloc::from_owner(&self.alloc)
    }
}

pub struct LuaFunctionCallInfo {
    pub savedpc: Instruction,
    pub trap: bool,
    pub nextraargs: i32,
}

pub enum CallInfoU {
    LuaFunction(LuaFunctionCallInfo),
    CFunction(),
}

pub struct CallInfo {
    pub previous: Option<Box<CallInfo>>,
    pub top: usize,
    pub func: usize,
    pub u: CallInfoU,
}

// ============================================================================
// LuaState — 单线程/单协程的 VM 执行上下文
// ============================================================================
//
// 对应 C Lua 中每个 lua_State 独立的执行状态 (stack / ci 链 / openupval /
// hook 等)。每个 LuaState 都是独立的执行上下文（协程 exec 持久驻留
// ThreadContext.exec，Box 地址稳定，运行/挂起同一份）；跨协程共享的
// 字段 (registry/globals/gc 等) 留在 GlobalState，通过 `bind_g` 绑定访问。

#[derive(Debug)]
pub struct LuaState<'a> {
    // Rc<Vec> 避免 op_call 中深拷贝 proto 字段（perf: 省 ~5.3% malloc+memmove）
    pub constants: Rc<Vec<TValue<'a>>>,
    pub code: Rc<Vec<Instruction>>,
    /// 当前执行函数的子原型列表 — Rc 共享，op_call 切换 proto 时 O(1) 引用计数
    pub protos: Rc<Vec<Rc<Proto<'a>>>>,
    pub top: usize,
    pub base: usize,
    pub pc: usize,
    pub num_params: u8,
    pub is_vararg: bool,
    /// 当前执行函数原型的 flag（PF_VAHID / PF_VATAB / PF_FIXED）
    pub proto_flag: u8,
    /// PF_VAHID 模式下隐藏变参的数量（对应 C 的 ci->u.l.nextraargs）
    pub nextraargs: i32,
    pub closure_upvals: Rc<RefCell<UpValVec<'a>>>,
    /// 全局 open upvalue 存储 — 不随函数调用/返回保存/恢复（对应 C 的 L->openupval 链表节点存储）
    /// open_upval 链表索引此 vec，tbc_list 也索引此 vec。
    ///
    /// 槽位在 upvalue 关闭（`func::close_upval`）时被释放为 `None` 并回收进 `free_upvals`，
    /// 使已关闭的 upvalue 不再被 arena 强引用（否则其内存与其 Closed 值会永久泄漏）。
    /// 索引在 upvalue 存活期间保持稳定 —— 未被释放的槽位永远不会被改写。
    pub open_upvals: Vec<Option<UpValRef<'a>>>,
    /// `open_upvals` 的空闲槽位索引（后进先出复用以保持局部性）。
    pub free_upvals: Vec<usize>,
    pub open_upval: Option<usize>,
    pub tbc_list: Option<usize>,
    pub call_stack: Vec<crate::objects::CallFrame<'a>>,
    pub stack: Vec<TValue<'a>>,
    pub hook_old_pc: i32,
    /// debug hook 函数 — 对应 C 的 L->hook
    pub hook_func: Option<TValue<'a>>,
    /// debug hook 掩码 — 对应 C 的 L->hookmask
    pub hook_mask: i32,
    /// debug hook count — 对应 C 的 L->basehookcount
    pub hook_count: i32,
    /// 当前 hook count (每条指令递减) — 对应 C 的 L->hookcount
    pub current_hook_count: i32,
    /// 是否允许调用 hook — 对应 C 的 L->allowhook
    pub allowhook: bool,
    /// 当前活动协程的上下文 — None 表示主线程执行中。
    ///
    /// **必须是 `Weak`（非 `Rc`）**：本执行态（协程的 `lua_State`）由
    /// `ThreadContext.exec` 内联持有，若此处反向持强引用即构成自引用环
    /// `ThreadContext --(exec)--> LuaState --(current_thread)--> ThreadContext`，
    /// 引用计数永不归零 —— 任何 resume 过的协程（无论从此结束还是挂起、
    /// 之后是否仍可达）都会永久泄漏整个执行态及其栈/upvalue 引用的对象图。
    ///
    /// 读取一律走 [`LuaState::current_thread_ctx`]（`upgrade`）。执行期间
    /// ThreadContext 必被强引用（`LuaThread.context` 持有，而线程对象由调用方栈 /
    /// wrap RustClosure upvalue / C 侧句柄强持有），故 upgrade 必定成功；
    /// 空闲期 upgrade 失败只发生在该执行态已随 ThreadContext 一起析构的场合。
    pub current_thread: Option<Weak<RefCell<ThreadContext<'a>>>>,
    /// 调用栈 — 保存 caller 的 VM 执行上下文（原 execute_loop 局部变量，提升为字段以支持协程挂起）
    pub call_info: Vec<CallInfoEntry<'a>>,
    /// pcall 保护栈 — 对应 C Lua 的 CIST_YPCALL CallInfo 链
    pub pcall_protection_stack: Vec<PcallProtection<'a>>,
    /// close continuation 的 pending error — 对应 C Lua 的 CIST_RECST 保存的错误状态
    pub close_error_status: Option<TValue<'a>>,
    /// coroutine.close() 关闭自身时设置 — 对应 C Lua 的 lua_closethread(co, L) 场景
    pub force_noyield_close: bool,
    /// C 函数调用嵌套计数（对应 C 的 L->nCcalls），用于检测 C 栈溢出
    pub n_ccalls: u32,
    /// 外部 C 帧（CClosure/LCFn，即 dlopen 加载的 C 模块或宿主 C 函数）的嵌套深度。
    ///
    /// C 代码只经 C API 压栈 —— `lua_pushxxx` 只增长 `stack.len()`，**不推进 `state.top`**
    /// （C 函数内 `top` 停在参数末尾）。因此只要该深度 > 0，GC 根扫描就必须覆盖到
    /// `stack.len()`，否则 C 调用期间触发的 GC 会把 C 代码刚压入的活值（如 lua_cjson_new
    /// 建到一半的模块表）当作垃圾清扫 —— CI 上 lua-cjson `cjson.safe` 上值变 nil、
    /// skynet e2e 失败即此。
    pub c_frame_depth: u32,
    /// 非可 yield 调用计数（对应 C 的 nCcalls 高 16 位 / incnny）
    pub n_ny_calls: u32,
    /// yield 调用的 nresults（恢复时用于调整 resume 参数数量）
    /// 仅在 started=true 且上次是 yield 挂起时有意义
    pub saved_yield_nresults: i32,
    pub api_func_base: usize,

    /// 最后一次错误的堆栈回溯字符串
    pub last_traceback: String,

    /// 最后一次错误的格式化消息（含 source:line 前缀）
    pub last_error_msg: String,

    /// 最后一次错误的原始值（保留原始 TValue 类型，如 error(100) 的数字 100）
    /// coroutine.close 需要返回此原始值
    pub last_error_value: Option<TValue<'a>>,

    /// C API 的 lua_error 抛错暂存槽 — 对应 C 的 longjmp 错误值
    /// lua_error 设置此字段并 panic，pcall_c_function 用 catch_unwind 捕获后取出
    pub pending_error: Option<TValue<'a>>,

    /// 标记错误消息是否应跳过 source:line 前缀
    /// error() level=0 或非字符串错误值时为 true，build_traceback 不再添加前缀
    pub error_no_prefix: bool,

    /// pcall 内部发生 yield 时暂存的 yield 值，供调用者传播
    pub pending_yield: Option<Vec<TValue<'a>>>,

    /// call_wrap_fn 执行期间，调用者栈（含活跃的 wrap RustClosure 引用）暂存于此，
    /// 让 GC 能看到内层协程引用，避免误判为不可达。
    /// 嵌套 wrap 调用时按栈顺序 push/pop。
    pub caller_gc_stacks: Vec<Vec<TValue<'a>>>,

    /// hook 传输信息 — 对应 C 的 L->transferinfo
    /// 记录最近一次 call/return hook 传输的值的位置和数量
    /// debug.getinfo 的 'r' 选项从此字段读取 ftransfer/ntransfer
    pub transferinfo_ftransfer: i32,

    pub transferinfo_ntransfer: i32,

    /// 待执行的返回值调整 — 当 return hook 启用时，push_results 不立即 adjust 栈,
    /// 而是把结果放到栈上并设置此字段。op_call 在 return hook 执行完后执行 adjust。
    /// (a, nresults, n_actual, first_result_pos) — a=func 位置, nresults=期望返回值数,
    /// n_actual=实际返回值数, first_result_pos=结果在栈上的起始位置
    pub pending_return_adjust: Option<(usize, i32, usize, usize)>,

    /// 错误发生时的 call_info 快照 — 对应 C Lua 中 longjmp 后 CallInfo 链表保持完整的行为
    /// state.pcall 错误时在 truncate call_info 之前保存快照。
    /// call_xpcall 调用错误处理函数之前恢复此快照，让 debug.traceback 能看到错误发生时的帧
    /// (如 __close 帧)。调用错误处理函数后清除。
    pub last_error_call_info: Option<Vec<CallInfoEntry<'a>>>,

    /// 最后一个出错的 __close 的 CallInfoEntry — 对应 C Lua 中 longjmp 跳过 callclosemethod
    /// 的弹出代码，CallInfo 节点保留在链表中。call_close_method 出错时 pop __close 帧并保存
    /// 到此字段。state.pcall 保存 call_info 快照时将此帧追加到快照末尾，让 xpcall 的错误
    /// 处理函数（如 debug.traceback）能看到 __close 帧。
    pub last_close_frame: Option<CallInfoEntry<'a>>,

    /// C 安全 keepalive 区 — pcall 错误路径中被截断的 TValue 暂存于此，
    /// 延迟到下次 lua_pcallk 调用时释放。
    ///
    /// 原因：C 代码（如 lsqlite3 的 db_sql_normal_function）在 lua_pcall 失败后
    /// 可能仍持有 userdata 指针（通过 lua_newuserdatauv 获取），并访问其数据区
    /// （如 `ctx->ctx = NULL`）。C Lua 的栈是数组，truncate 只移动 top 指针，
    /// userdata 在 GC 前保持有效。Rust 用 Vec<TValue>，truncate 立即 drop，
    /// Rc 引用计数为 0 时 userdata 内存被释放，导致悬空指针。
    /// 此字段模拟 C Lua 的 GC 延迟释放行为。
    ///
    /// 仅 `lua_pcallk` 入口（`c_pcall_stash` 为 true）暂存；Lua 层 `pcall`/`xpcall`
    /// 等纯 Lua 调用方不持有 C 指针，直接截断，避免暂存区无界增长（泄漏）。
    pub c_safety_keepalive: Vec<TValue<'a>>,

    /// 当前 `LuaState::pcall` 是否由 C API（`lua_pcallk`）发起。
    /// 只有 C 发起时错误路径才把被截断的值移入 `c_safety_keepalive`。
    pub c_pcall_stash: bool,

    /// setjmp/longjmp 缓冲区栈 — 用于 lua_use_longjmp 模式下
    /// C 函数错误处理，替代 catch_unwind。
    /// 每个元素指向调用方栈上的 [u8; 512] 缓冲区 (>= sizeof(jmp_buf))。
    /// 仅在 lua_use_longjmp 模式下使用 (size_optimized 自动启用, 或 --features lua_longjmp)。
    pub error_jmp_bufs: Vec<*mut core::ffi::c_void>,

    /// 对应 C 的 `lua_State.l_G`：指向拥有本执行态的 `GlobalState`（即共享全局态）。
    /// 裸指针，进入执行前用 `bind_g` 重绑定；规约见下方 impl 注释 R1/R2/R3。
    pub l_g: *mut GlobalState<'a>,

    /// 主线程持有共享全局块 —— 对应 C 的 `lua_newstate` 里 LG 单次分配
    /// （主线程 `lua_State` 与 `global_State` 同块）。协程为 `None`（`l_g` 指向主人的块）。
    ///
    /// `Box` 保证 `l_g` 指向的地址不随 `LuaState` 自身移动而失效；
    /// 模块私有（`GlobalState` 的构造与所有权均不外泄，其他模块只能用 `LuaState`）。
    owner: Option<Box<GlobalState<'a>>>,
}

impl<'a> LuaState<'a> {
    /// 协程/新线程的执行态骨架（对应 C 中为新线程分配的 `lua_State`）：
    /// 独立字段全默认，不持有共享块；`l_g` 由调用方 `bind_g` 绑定。
    pub fn empty_thread() -> Self {
        LuaState {
            constants: Rc::new(Vec::new()),
            code: Rc::new(Vec::new()),
            protos: Rc::new(Vec::new()),
            top: 0,
            base: 0,
            pc: 0,
            num_params: 0,
            is_vararg: false,
            proto_flag: 0,
            nextraargs: 0,
            closure_upvals: Rc::new(RefCell::new(UpValVec::new())),
            open_upvals: Vec::new(),
            free_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            call_stack: Vec::new(),
            stack: Vec::new(),
            hook_old_pc: 0,
            hook_func: None,
            hook_mask: 0,
            hook_count: 0,
            current_hook_count: 0,
            allowhook: true,
            current_thread: None,
            call_info: Vec::new(),
            pcall_protection_stack: Vec::new(),
            close_error_status: None,
            force_noyield_close: false,
            n_ccalls: 0,
            c_frame_depth: 0,
            n_ny_calls: 0,
            saved_yield_nresults: 0,
            api_func_base: 0,
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_error_value: None,
            pending_error: None,
            error_no_prefix: false,
            pending_yield: None,
            caller_gc_stacks: Vec::new(),
            transferinfo_ftransfer: 0,
            transferinfo_ntransfer: 0,
            pending_return_adjust: None,
            last_error_call_info: None,
            last_close_frame: None,
            c_safety_keepalive: Vec::new(),
            c_pcall_stash: false,
            error_jmp_bufs: Vec::new(),
            l_g: std::ptr::null_mut(),
            owner: None,
        }
    }
}

/// 主线程构造（对应 C 的 `lua_newstate`）：创建并持有共享全局块。
/// 使用默认 IO（`lua_io()`）；自定义 IO 用 `LuaState::new_main`。
impl Default for LuaState<'static> {
    fn default() -> Self {
        Self::new_main(crate::mock::io_mock::lua_io())
    }
}

impl<'a> LuaState<'a> {
    /// 创建主线程执行态并持有共享全局块 —— 对应 C 的 `lua_newstate`（主线程 + global_State
    /// 同块分配）。栈初始化对应 C 的 `stack_init`：`stack[0] = nil`（函数入口），`top = 1`。
    ///
    /// 这是**唯一**创建 `GlobalState` 的入口（其它模块无法构造 `GlobalState`）；协程执行态
    /// 一律用 `empty_thread()` + `bind_g()` 共享主人的块。
    ///
    /// Rust API 入口：`allocator` 由调用方选择 — `Box::new(DefaultAllocator)` 或任意
    /// 自定义 `Allocator` 实现。C API 路径（lua_newstate）经由 setallocf 语义注入
    /// `CapiAllocator` 包装宿主的 lua_Alloc。
    pub fn new_main_with_allocator(
        io: &'a mut dyn crate::mock::io_mock::Io,
        allocator: Box<dyn Allocator>,
    ) -> Self {
        let owner = Box::new(GlobalState::new_shared(io, allocator));
        let mut s = Self::empty_thread();
        s.stack = GlobalState::init_stack();
        s.top = s.stack.len();
        s.owner = Some(owner);
        s.bind_exec();
        s
    }

    /// 同 `new_main_with_allocator`，使用 `DefaultAllocator`。
    pub fn new_main(io: &'a mut dyn crate::mock::io_mock::Io) -> Self {
        Self::new_main_with_allocator(io, Box::new(DefaultAllocator))
    }

    /// 进入执行前重绑定（规约 R1）：主线程以自有共享块重绑并登记 `G.main`，
    /// 协程以 `l_g` 指向的共享块重绑。所有进入点（execute/pcall/resume/capi 入口）
    /// 必须先调用本方法；`LuaState` 移动后旧指针失效，靠重新绑定自愈。
    #[inline]
    pub fn bind_exec(&mut self) {
        // 取裸指针不经 `&mut` 引用：l_g 的 tag 保持 Reserved（interior mutable），
        // Tree Borrows 下不被主线程经 owner 的 foreign write 禁用。
        let g: *mut GlobalState<'a> = match self.owner.as_mut() {
            Some(b) => std::ptr::addr_of_mut!(**b),
            None => return, // 协程：l_g 已由调用方（bind_g）绑定
        };
        self.l_g = g;
        // 主线程登记 G.main（对应 C 的 G.mainthread）：GC 需把主线程栈当根。
        // 经 owner 裸指针直接写（不经 l_g），避免把 l_g 的 tag 提升为 Active。
        let self_ptr: *mut LuaState<'a> = self;
        unsafe { (*g).main = self_ptr };
    }
}

impl<'a> Drop for LuaState<'a> {
    fn drop(&mut self) {
        if self.owner.is_some() {
            self.bind_exec();
            self.caller_gc_stacks.clear();
            self.c_safety_keepalive.clear();
            self.close_state();
        }
    }
}

// ============================================================================
// LuaState 的 l_G 桥（对应 C 的 lua_State.l_G / G(L)）
// ============================================================================
//
// 规约（务必遵守，否则可能未定义行为或静默错误）：
//   R1: l_g 仅在"活跃进入点"内有效。所有进入点（GlobalState::execute / pcall /
//       capi lua_newstate / lua_newthread / 协程 resume）必须调用 bind_g 重绑定。
//       移动 GlobalState 后旧指针失效，靠进入时重绑定自愈。
//   R2: 持有 &mut LuaState 期间禁止重新赋值共享块（G.exec 已随 Design C 迁移删除，
//       GlobalState 不再持有 exec；但 GlobalState 方法仍不得访问调用者的 LuaState）——
//       执行路径上调用者持 `&mut LuaState`，共享块方法再取同一对象 `&mut` 就是 UB。
//   R3: DerefMut 已删除；只保留只读 Deref，共享字段写访问改用 g_mut()。
impl<'a> LuaState<'a> {
    /// 对应 C 的 `G(L)`：取共享全局态（只读）。
    ///
    /// 与 `g_mut` 同理：主线程优先走 `owner`（新派生，不受返回边界 retag 影响），
    /// 协程（无 owner）走 `l_g`。
    #[inline]
    pub fn g(&self) -> &GlobalState<'a> {
        debug_assert!(!self.l_g.is_null(), "LuaState::l_g 未绑定（缺少 bind_g）");
        match self.owner.as_ref() {
            Some(b) => &**b,
            None => unsafe { &*self.l_g },
        }
    }

    /// 对应 C 的 `G(L)`：取共享全局态（可变）。
    ///
    /// 主线程优先走 `owner`（每次从 `&mut **b` 新派生指针）：`new_main` 返回 `s` 时
    /// owner 字段被 retag，`l_g` 里旧 tag 失效（Miri Stacked Borrows 实证
    /// capi::lua_newstate 路径 retag error）；协程（无 owner）才走 `l_g`。
    #[inline]
    pub fn g_mut(&mut self) -> &mut GlobalState<'a> {
        debug_assert!(!self.l_g.is_null(), "LuaState::l_g 未绑定（缺少 bind_g）");
        match self.owner.as_mut() {
            Some(b) => &mut **b,
            None => unsafe { &mut *self.l_g },
        }
    }

    /// 绑定 `l_g`。每次进入执行前调用。
    #[inline]
    pub fn bind_g(&mut self, g: *mut GlobalState<'a>) {
        self.l_g = g;
    }

    /// 当前活动协程的上下文（`current_thread` 的 upgrade 形式）。
    ///
    /// 执行期间必定返回 `Some`（见字段文档）；空闲期若协程已随线程对象析构则返回 `None`。
    #[inline]
    pub fn current_thread_ctx(&self) -> Option<Rc<RefCell<ThreadContext<'a>>>> {
        self.current_thread.as_ref().and_then(|w| w.upgrade())
    }

    /// 对应 C 的 `G(L)->gc`：取 GC 状态裸指针（不产生借用，便于与栈借用共存）。
    ///
    /// 直接从共享块读 GC 状态字段指针，不经 `g()` 造的 `&GlobalState`——共享引用派生出的
    /// 写指针在 LLVM 下是 UB（noalias），release 会误优化（实测 table.sort 错误值丢失）。
    ///
    /// 主线程优先走 `owner`（每次从 `&mut **b` 新派生，不受返回边界 retag 影响，
    /// 同 `g()`/`g_mut()` 的修法）；协程（无 owner）才走 `l_g`。
    #[inline]
    pub unsafe fn gc_ptr(&self) -> *mut GCState<'a> {
        match self.owner.as_ref() {
            Some(b) => (*b).gc.as_ptr(),
            None => (*self.l_g).gc.as_ptr(),
        }
    }

    /// 对应 C 的 `G(L)->gc`：GC 状态可变访问（C 语义的内部可变，仅借 `&self`）。
    /// 热路径上避免 `state.stack` 可变借用与 `G(L)->gc` 访问互相冲突（C 中
    /// `global_State *g = G(L)` 是独立指针，无别名约束）。
    #[inline]
    pub unsafe fn gc_mut(&self) -> &mut GCState<'a> {
        &mut *self.gc_ptr()
    }

    /// 【测试专用】共享块地址（验证多协程 l_g 指向同一 GlobalState）。
    #[cfg(test)]
    pub fn g_addr(&self) -> usize {
        match self.owner.as_ref() {
            Some(b) => &**b as *const GlobalState<'a> as usize,
            None => self.l_g as usize,
        }
    }

    /// 【测试专用】从 l_g 取共享块引用（测试模块跨文件验证 globals 共享）。
    #[cfg(test)]
    pub fn g_ref_test(&self) -> &GlobalState<'a> {
        debug_assert!(!self.l_g.is_null(), "l_g 未绑定");
        unsafe { &*self.l_g }
    }
}

/// 【测试专用】globals 表地址（验证协程与主线程共享同一 Table 对象）。
#[cfg(test)]
impl<'a> GlobalState<'a> {
    pub fn globals_addr(&self) -> usize {
        &self.globals as *const Table<'a> as usize
    }
}

impl<'a> std::ops::Deref for LuaState<'a> {
    type Target = GlobalState<'a>;
    #[inline]
    fn deref(&self) -> &GlobalState<'a> {
        self.g()
    }
}

impl<'a> std::ops::DerefMut for LuaState<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut GlobalState<'a> {
        self.g_mut()
    }
}

// ---------------------------------------------------------------------------
// 进入点绑定（规约 R1）：pcall / execute 等进入执行前统一 bind_exec，
// 使方法内的 `G(L)->` 访问有效（构造后移动了共享块，旧指针靠进入时重绑定自愈）。
// ---------------------------------------------------------------------------

impl<'a> LuaState<'a> {
    /// 打开选定的标准库 — 对应 C 的 `luaL_openselectedlibs(L, load, preload)`。
    /// 标准库开库需要每线程执行态（LuaState = C 的 lua_State）。
    pub fn open_selected_libs(&mut self, _mask: i32, _ignored: i32) {
        let arg_table = Table::new_in(self.alloc_slot());
        unsafe { self.gc_mut().register_table(&arg_table) };
        self.globals
            .set(self.intern_str("arg"), TValue::Table(arg_table));

        // 打开基础库 (注册 print, type, pcall, error, setmetatable, getmetatable,
        // tonumber, tostring, assert, select, rawequal, rawlen, rawget, rawset,
        // next, ipairs, pairs, xpcall, warn, _G, _VERSION 等全局函数)
        crate::stdlib::base_lib::open_base_lib(self);

        // 打开字符串库 (创建字符串元表)
        crate::stdlib::string_lib::open_string_lib(self);

        // 打开数学库 (注册 math 全局表, 包含 abs/sin/cos/random 等)
        crate::stdlib::math_lib::open_math_lib(self);

        // 打开 UTF-8 库 (注册 utf8 全局表, 包含 offset/codepoint/char/len/codes 等)
        crate::stdlib::utf8_lib::open_utf8_lib(self);

        // 打开 Table 库 (注册 table 全局表, 包含 concat/unpack/pack/insert/remove 等)
        crate::stdlib::table_lib::open_table_lib(self);

        // 打开 Debug 库 (注册 debug 全局表, 包含 getinfo/getlocal/setupvalue/traceback/sethook 等)
        crate::stdlib::debug_lib::open_debug_lib(self);

        // 打开 OS 库 (注册 os 全局表, 包含 setlocale 等)
        crate::stdlib::os_lib::open_os_lib(self);

        // 打开 Coroutine 库 (注册 coroutine 全局表, 包含 create/resume/yield/status 等)
        crate::stdlib::coroutine_lib::open_coroutine_lib(self);

        // 打开 I/O 库 (注册 io 全局表, 包含 stdin/stdout/stderr)
        // Miri 不支持 extern static stdin/stdout/stderr 与 fdopen,跳过 io 库初始化
        // (print 用 state.stdout 而非 io.stdout,基础测试不受影响)
        #[cfg(not(miri))]
        crate::stdlib::io_lib::open_io_lib(self);

        // 把标准库注册到 package.loaded（对应 C 的 luaL_requiref），
        // 防止 nextvar.lua 清理全局表时把标准库当作未加载模块删除
        for name in [
            "string",
            "math",
            "utf8",
            "table",
            "debug",
            "os",
            "coroutine",
            "io",
            "package",
        ] {
            let name_val = self.intern_str(name);
            if let Some(lib) = self.globals.get(&name_val) {
                let package_key = self.intern_str("package");
                if let Some(TValue::Table(pkg)) = self.globals.get(&package_key) {
                    let loaded_key = self.intern_str("loaded");
                    if let Some(TValue::Table(loaded)) = pkg.get(&loaded_key) {
                        loaded.set(name_val, lib);
                    }
                }
            }
        }
    }
}

// ============================================================================
// LuaState — 每线程执行态 API（对应 C 的 lua_State 层接口）
// ============================================================================

impl<'a> LuaState<'a> {
    fn condmovestack(&mut self, _pre: usize, _pos: usize) {
        // Rust 版本: Vec 自行管理内存，无需在栈重分配时修正指针
        // C 版本的 condmovestack 仅在 hardstacktests 配置下做额外检查
    }

    pub fn checkstackaux(&mut self, n: usize, pre: usize, pos: usize) {
        if self.stack.len() - self.top <= n {
            let _ = self.growstack(n, true);
        } else {
            self.condmovestack(pre, pos);
        }
    }

    pub fn checkstack(&mut self, n: usize) {
        self.checkstackaux(n, 0, 0);
    }

    /// 对应 C 的 luaD_growstack
    /// 尝试将栈增长至少 n 个元素。raiseerror=true 时报告错误，否则返回错误。
    pub fn growstack(&mut self, n: usize, raiseerror: bool) -> Result<(), VmError<'a>> {
        let size = self.stack.len();
        if size > MAXSTACK {
            // 栈已超过最大值，线程正在使用为错误保留的额外空间
            debug_assert_eq!(size, ERRORSTACKSIZE);
            return Err(VmError::StackError);
        } else if n < MAXSTACK {
            let mut newsize = size + (size >> 1); /* tentative new size (size * 1.5) */
            let needed = self.top + n;
            if newsize > MAXSTACK {
                newsize = MAXSTACK;
            }
            if newsize < needed {
                newsize = needed;
            }
            if newsize <= MAXSTACK {
                return self.reallocstack(newsize, raiseerror);
            }
        }
        /* else stack overflow */
        /* add extra size to be able to handle the error message */
        self.reallocstack(ERRORSTACKSIZE, raiseerror)?;
        if raiseerror {
            runerror(self, "stack overflow", &[]);
        }
        Err(VmError::StackOverflow)
    }

    /// 对应 C 的 luaD_reallocstack
    /// Rust 版本: Vec 自行管理内存，relstack/correctstack 为空操作
    pub fn reallocstack(&mut self, newsize: usize, _raiseerror: bool) -> Result<(), VmError<'a>> {
        let oldsize = self.stack.len();
        debug_assert!(newsize <= MAXSTACK || newsize == ERRORSTACKSIZE);
        // relstack: Rust 中无需将指针转为偏移量 (Vec 自行管理内存)
        // G(self).gcstopem = true: 简化，不停止紧急 GC
        // 扩展栈到 newsize + EXTRA_STACK，新位置填 nil
        let target = newsize + EXTRA_STACK;
        if target > oldsize {
            self.stack.resize(target, TValue::Nil(NilKind::Strict));
        }
        // correctstack: Rust 中无需修正指针 (使用索引而非指针)
        Ok(())
    }
}

// ============================================================================
// LuaState — adjust_results 等
// ============================================================================

impl<'a> LuaState<'a> {
    /// 调整 C 函数返回值到栈上 — 对应 C 的 luaD_poscall 中的栈调整
    ///
    /// 当 return hook 启用时，不立即 adjust，而是把结果放到栈顶之上，
    /// 设置 pending_return_adjust，由 op_call 在 return hook 后执行 adjust。
    /// 这样 return hook 中的 debug.getlocal 能从栈上读到返回值。
    pub fn adjust_results(&mut self, a: usize, nresults: i32, results: Vec<TValue<'a>>) {
        // 如果 return hook 启用且允许调用 hook，先把 results 放到栈顶之上（不 truncate），
        // 设置 pending_return_adjust，由 op_call 在 return hook 后执行 adjust。
        // hook 执行期间 allowhook=false，return hook 不会被调用，不应推迟。
        // 只有在 op_call 路径中（call_info 栈顶有 is_c=true 的 entry）才推迟，
        // 因为只有 op_call 会调用 finish_pending_adjust。单元测试直接调用 C 函数时
        // call_info 为空，不应推迟。
        if self.hook_mask & 2 != 0 && self.allowhook {
            // LUA_MASKRET
            let in_op_call = self.call_info.last().map(|e| e.is_c).unwrap_or(false);
            if in_op_call {
                let first_result_pos = self.stack.len();
                // perf: 用 into_iter drain 避免 clone (每个 results[i].clone() 对 Rc 变体需 incq)
                let n = results.len();
                self.stack.extend(results);
                self.pending_return_adjust = Some((a, nresults, n, first_result_pos));
                self.top = self.stack.len();
                return;
            }
        }

        self.stack.truncate(a);
        let n = if nresults < 0 {
            results.len()
        } else {
            nresults as usize
        };
        // perf: 用 into_iter drain 避免 clone。n <= results.len() 时直接 drain 前 n 个;
        // n > results.len() 时 drain 全部后补 Nil。
        let rlen = results.len();
        if n <= rlen {
            self.stack.extend(results.into_iter().take(n));
        } else {
            self.stack.extend(results);
            for _ in rlen..n {
                self.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        // 关键: 更新 state.exec.top — 后续 MULTRET CALL (b=0) 用 state.exec.top 计算 nargs,
        // 若不更新, state.exec.top 保留 CALL 设置的 a+b (过大), 导致 nargs 计算错误
        self.top = self.stack.len();
    }

    /// adjust_results 的单值版本 — 消除 vec![value] 的堆分配。
    /// 热路径（find 未命中 / gmatch 迭代结束 / 字符串库单返回值）每调用一次，
    /// 1 元素 Vec 分配 + move 开销不可忽略（D 循环基准中每 find 一次）。
    #[cfg_attr(not(size_optimized), inline)]
    pub fn adjust_single_result(&mut self, a: usize, nresults: i32, result: TValue<'a>) {
        if self.hook_mask & 2 != 0 && self.allowhook {
            let in_op_call = self.call_info.last().map(|e| e.is_c).unwrap_or(false);
            if in_op_call {
                let first_result_pos = self.stack.len();
                self.stack.push(result);
                self.pending_return_adjust = Some((a, nresults, 1, first_result_pos));
                self.top = self.stack.len();
                return;
            }
        }
        // perf: 热路径直写 — truncate+push 改为覆写函数槽 + 单次截断。
        // 调用点 op_call 时 stack 长度 = a+1+nargs (函数+参数), 结果直写 a,
        // 多余参数槽由 truncate(a+1) 逐槽正常 drop (单参时 1 槽, trivial)。
        // 对应 C 的 setobj2s(L, func_slot, result); L->top = func+2。
        if nresults <= 0 {
            if nresults == 0 {
                self.stack.truncate(a);
            } else {
                // MULTRET (nresults < 0): 单值即全部结果
                if self.stack.len() > a {
                    self.stack[a] = result;
                    self.stack.truncate(a + 1);
                } else {
                    self.stack.truncate(a);
                    self.stack.push(result);
                }
            }
        } else if (nresults as usize) == 1 {
            if self.stack.len() > a {
                // fast_unsafe: 槽位平凡值检查 + 尾部 set_len (免 drop_glue)
                #[cfg(feature = "fast_unsafe")]
                {
                    // perf: 槽位平凡值检查 — 槽 a 现值多为 BuiltinFn (函数槽,
                    // Copy 无 drop) 或 trivial; 平凡/Copy 槽用 ptr::write 跳过
                    // drop_in_place 调用 (非内联 call, adjust 热路径实证)。
                    // 非平凡 (Rc/Table 等) 保守走原赋值 (正常 drop)。
                    // 检查用判别范围: BuiltinFn 及其他 Copy 变体由 matches 显式列出。
                    let slot = unsafe { self.stack.get_unchecked_mut(a) };
                    if matches!(
                        slot,
                        TValue::Nil(_)
                            | TValue::Boolean(_)
                            | TValue::Integer(_)
                            | TValue::Float(_)
                            | TValue::BuiltinFn(_)
                    ) {
                        // SAFETY: 槽现值无 drop glue (Copy 变体), ptr::write 覆盖安全
                        unsafe { std::ptr::write(slot, result) };
                    } else {
                        *slot = result;
                    }
                    // perf: 被截断区 (a+1..len) 是 CALL 的参数槽, 常为 trivial (Float/
                    // Integer)。全部 trivial 时用 set_len 跳过 Vec::truncate 的逐槽
                    // drop glue (非内联调用); 有 Rc 变体则回退 truncate 正常 drop。
                    let tail_all_trivial = self.stack[a + 1..].iter().all(|v| {
                        matches!(
                            v,
                            TValue::Nil(_)
                                | TValue::Boolean(_)
                                | TValue::Integer(_)
                                | TValue::Float(_)
                        )
                    });
                    if tail_all_trivial {
                        // SAFETY: 尾部全 trivially-droppable, set_len 略过 drop 安全;
                        // 槽位保留给后续 write_stack 复用 (与 smart_clear_stack 同策略)。
                        unsafe { self.stack.set_len(a + 1) };
                    } else {
                        self.stack.truncate(a + 1);
                    }
                }
                // 安全版: 常规赋值 + truncate (旧值走 drop_glue)
                #[cfg(not(feature = "fast_unsafe"))]
                {
                    self.stack[a] = result;
                    self.stack.truncate(a + 1);
                }
            } else {
                self.stack.push(result);
            }
        } else {
            self.stack.truncate(a);
            self.stack.push(result);
            for _ in 1..nresults {
                self.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        self.top = self.stack.len();
    }

    /// adjust_results 的双值版本 — 消除 vec![v1, v2] 堆分配（gsub 返回 热点）。
    pub fn adjust_two_results(&mut self, a: usize, nresults: i32, v1: TValue<'a>, v2: TValue<'a>) {
        if self.hook_mask & 2 != 0 && self.allowhook {
            let in_op_call = self.call_info.last().map(|e| e.is_c).unwrap_or(false);
            if in_op_call {
                let first_result_pos = self.stack.len();
                self.stack.push(v1);
                self.stack.push(v2);
                self.pending_return_adjust = Some((a, nresults, 2, first_result_pos));
                self.top = self.stack.len();
                return;
            }
        }
        if nresults == 0 {
            self.stack.truncate(a);
        } else {
            self.stack.truncate(a);
            self.stack.push(v1);
            if nresults == 1 {
                self.top = self.stack.len();
                return;
            }
            self.stack.push(v2);
            if nresults > 2 {
                for _ in 2..nresults {
                    self.stack.push(TValue::Nil(NilKind::Strict));
                }
            }
        }
        self.top = self.stack.len();
    }

    /// 与 adjust_results 类似，但结果已在栈上 [first_result_pos..first_result_pos+n_actual)。
    /// 避免创建临时 Vec（对 table.unpack 等大量结果的场景至关重要，防止 OOM）。
    pub fn adjust_results_on_stack(
        &mut self,
        a: usize,
        nresults: i32,
        n_actual: usize,
        first_result_pos: usize,
    ) {
        if self.hook_mask & 2 != 0 && self.allowhook {
            let in_op_call = self.call_info.last().map(|e| e.is_c).unwrap_or(false);
            if in_op_call {
                self.pending_return_adjust = Some((a, nresults, n_actual, first_result_pos));
                return;
            }
        }
        // 将结果从 first_result_pos 移动到 a
        // 用 mem::replace 避免 clone：源位置的值被移出（填 Nil(Empty)），
        // 后续 truncate 会删除源位置，不会留下垃圾。
        // 正向循环安全：a < first_result_pos，写 a+k 不会覆盖尚未读取的源 first_result_pos+k
        if n_actual > 0 && first_result_pos + n_actual <= self.stack.len() {
            for k in 0..n_actual {
                let val = std::mem::replace(
                    &mut self.stack[first_result_pos + k],
                    TValue::Nil(NilKind::Empty),
                );
                self.stack[a + k] = val;
            }
        }
        let new_len = if nresults < 0 {
            a + n_actual
        } else {
            let nr = nresults as usize;
            if nr > n_actual {
                // perf: 直接在 a+n_actual..a+nr 范围填 Nil，避免 truncate+push 的双次操作。
                // truncate 会 drop 多余元素然后可能缩容，push 又可能扩容。
                // 直接 resize 一次完成（resize 内部对增长部分用 Nil 填充）。
                let target = a + nr;
                if target <= self.stack.len() {
                    // 栈够长: 只需覆盖 a+n_actual..a+nr 为 Nil
                    for k in n_actual..nr {
                        self.stack[a + k] = TValue::Nil(NilKind::Strict);
                    }
                } else {
                    // 栈不够长: 先截断到 a+n_actual，再 resize 到 a+nr
                    self.stack.truncate(a + n_actual);
                    self.stack.resize(target, TValue::Nil(NilKind::Strict));
                }
            }
            a + nr
        };
        self.stack.truncate(new_len);
        self.top = self.stack.len();
    }

    /// 执行待定的返回值调整 — 由 op_call 在 return hook 后调用
    pub fn finish_pending_adjust(&mut self) {
        if let Some((a, nresults, n_actual, first_result_pos)) = self.pending_return_adjust.take() {
            // 直接执行结果调整，不调用 adjust_results_on_stack（它会检查推迟条件，
            // 当 allowhook=true 时会重新设置 pending_return_adjust 而不执行实际调整，
            // 导致 pending_return_adjust 保留到下一个 C 函数调用的 finish_pending_adjust
            // 错误执行，把栈截断到错误的 a 值）
            if n_actual > 0 && first_result_pos + n_actual <= self.stack.len() {
                for k in 0..n_actual {
                    let val = std::mem::replace(
                        &mut self.stack[first_result_pos + k],
                        TValue::Nil(NilKind::Empty),
                    );
                    self.stack[a + k] = val;
                }
            }
            let new_len = if nresults < 0 {
                a + n_actual
            } else {
                let nr = nresults as usize;
                if nr > n_actual {
                    self.stack.truncate(a + n_actual);
                    for _ in n_actual..nr {
                        self.stack.push(TValue::Nil(NilKind::Strict));
                    }
                }
                a + nr
            };
            self.stack.truncate(new_len);
            self.top = self.stack.len();
        }
    }
}

// ============================================================================
// LuaState — 调用/错误保护 API（对应 C 的 luaD_call / lua_pcall / luaD_pcall）
// ============================================================================

impl<'a> LuaState<'a> {
    /// 对应 C 的 getCcalls: 获取 C 调用嵌套数 (低 16 位)
    pub fn get_ccalls(&self) -> u32 {
        self.n_ccalls & 0xffff
    }

    /// 对应 C 的 ccall: 调用函数 (无错误保护)
    /// Rust 版本: 简化实现，委托给 pcall 并忽略错误
    fn ccall(&mut self, nargs: usize, n_results: i32, inc: u32) {
        self.n_ccalls = self.n_ccalls.saturating_add(inc);
        if self.get_ccalls() >= LUAI_MAXCCALLS {
            // 对应 C 的 checkstackp + luaE_checkcstack
            // 简化: 仅检查栈空间
            self.checkstack(0);
            if self.get_ccalls() >= LUAI_MAXCCALLS {
                runerror(self, "C stack overflow", &[]);
            }
        }
        // 委托给 pcall 执行实际调用 (忽略错误)
        let _ = self.pcall(nargs, n_results, 0);
        self.n_ccalls = self.n_ccalls.saturating_sub(inc);
    }

    pub fn call(&mut self, nargs: usize, nresults: i32) {
        self.ccall(nargs, nresults, 1)
    }

    pub fn call_no_yield(&mut self, nargs: usize, nresults: i32) {
        // nyci = 0x10000 | 1 (C: lstate.h)
        let nyci: u32 = 0x10000 | 1;
        self.ccall(nargs, nresults, nyci);
    }

    pub fn pcall(&mut self, nargs: usize, nresults: i32, _errfunc: isize) -> i32 {
        // 进入执行前绑定 l_g（规约 R1）
        self.bind_exec();
        // func_idx 是函数在栈中的 0-based 绝对索引。
        // 栈布局: [... | func | arg1 | arg2 | ... | top]
        // func_idx = stack.len() - nargs - 1
        let func_idx = self.stack.len().saturating_sub(nargs + 1);
        if func_idx >= self.stack.len() {
            return ERR_RUN;
        }

        // __call 元方法链解析 (对应 C 的 tryfuncTM + precall 的 goto retry)
        // 当 func 是表且有 __call 元方法时,在 func_idx 位置插入元方法,
        // 原表和所有参数右移 1 位,使调用变为 __call(original_table, args...)。
        // 循环处理嵌套 __call 链,直到找到可调用对象或超过 MAX_CALL_CHAIN(15)。
        // (op_call/op_tailcall 已在 execute.rs 中处理此逻辑,但 pcall 直接
        //  调用 state.pcall,所以这里也需要相同的处理。)
        let mut chain_len: usize = 0;
        loop {
            let cur_val = self.stack[func_idx].clone();
            if let TValue::Table(ref t) = cur_val {
                let mt_opt = t.get_metatable();
                let call_fn = mt_opt.as_ref().and_then(|mt| {
                    let call_key = self.intern_str("__call");
                    mt.get(&call_key)
                });
                if let Some(call_fn) = call_fn {
                    // 在 func_idx 位置插入元方法,原 func 和参数右移 1 位
                    // 对应 C: for (p = L->top.p; p > func; p--) setobjs2s(L, p, p-1);
                    self.stack.insert(func_idx, call_fn.clone());
                    if chain_len >= MAX_CALL_CHAIN {
                        self.stack.truncate(func_idx);
                        self.push_string("'__call' chain too long");
                        return ERR_RUN;
                    }
                    chain_len += 1;
                    continue; // 对应 goto retry
                }
                // 表无 __call 元方法,报 "attempt to call a table value"
                self.stack.truncate(func_idx);
                let ty = self.typename(cur_val.ty());
                self.push_string(&format!("attempt to call a {} value", ty));
                return ERR_RUN;
            }
            break; // 非表类型,退出循环进入原有 match
        }

        let func_val = self.stack[func_idx].clone();
        match func_val {
            TValue::LClosure(closure) => {
                // 检查 C 调用深度 (对应 C 的 luaE_incCstack / luaE_checkcstack)
                // 每次 Lua 闭包调用递增 n_ccalls,达到 LUAI_MAXCCALLS(200) 时
                // 抛出可被 pcall 捕获的 "C stack overflow",防止 Rust 原生栈溢出。
                // 注意: 必须在 execute_loop 之前检查,否则递归仍会无限进行。
                self.n_ccalls = self.n_ccalls.saturating_add(1);
                let cc = self.get_ccalls();
                // 对应 C 的 luaE_checkcstack:
                // cc == LUAI_MAXCCALLS → "C stack overflow"
                // cc >= LUAI_MAXCCALLS * 11 / 10 → "error in error handling"
                // cc 在 (MAXCCALLS, MAXCCALLS*1.1) 之间 → 不触发错误（error handler 中的调用）
                if cc == LUAI_MAXCCALLS || cc >= LUAI_MAXCCALLS * 11 / 10 {
                    self.n_ccalls = self.n_ccalls.saturating_sub(1);
                    self.stack.truncate(func_idx);
                    if cc >= LUAI_MAXCCALLS * 11 / 10 {
                        self.push_string("error in error handling");
                    } else {
                        self.push_string("C stack overflow");
                    }
                    return ERR_RUN;
                }
                // 保存 n_ccalls，用于 error 路径恢复（对应 C Lua 的 longjmp 恢复 ci->nCcalls）
                // error() 抛出时，execute_loop 中 OP_CALL 递增的 n_ccalls 不会由 op_return 递减，
                // 导致计数失衡。非 yield 路径需恢复到 pcall 调用前的值。
                let saved_n_ccalls = self.n_ccalls.saturating_sub(1);

                let nargs_actual = self.stack.len().saturating_sub(func_idx + 1);
                let fsize = closure.proto.max_stack_size as usize;
                let nfixparams = closure.proto.num_params as usize;
                let proto_is_vararg = closure.proto.is_vararg();

                // 提前提取 proto 和 upvals 的 Rc 引用，使后续可以 move closure（而非 clone）
                // perf: 消除 CallInfoEntry.closure 的 Box 堆分配
                let proto = Rc::clone(&closure.proto);
                let upvals = Rc::clone(&closure.upvals);

                let saved_code = std::mem::take(&mut self.code);
                let saved_constants = std::mem::take(&mut self.constants);
                let saved_protos = std::mem::take(&mut self.protos);
                let saved_base = self.base;
                let saved_pc = self.pc;
                let saved_num_params = self.num_params;
                let saved_is_vararg = self.is_vararg;
                let saved_proto_flag = self.proto_flag;
                let saved_nextraargs = self.nextraargs;
                let saved_closure_upvals = std::mem::replace(
                    &mut self.closure_upvals,
                    Rc::new(RefCell::new(UpValVec::new())),
                );
                let saved_tbc_list = self.tbc_list.take();

                // 推入 call_info — 对应 C 的 luaD_precall 创建新 CallInfo
                // state.pcall 不是通过 OP_CALL 调用的（从 C 函数内部调用），
                // 不能用 get_func_name 获取函数名（state.exec.pc 指向调用 pcall/xpcall
                // 的指令而非调用 g 的指令），name/namewhat 设为空。
                // caller_source/caller_line 从当前 state（调用者的执行上下文）获取。
                //
                // 总是推入 call_info 条目（对应 C 的 luaD_precall 总是创建新 CallInfo）。
                // 当调用者是 Lua 函数时，source/line/base/saved_pc 来自调用者；
                // 当调用者是 C 函数时（如 docall 从主线程 base 调用），source="=[C]"，
                // line=-1，base=0，saved_pc=0。build_traceback 会跳过 is_c=true 的条目
                // （c_chain_len 机制），所以不会造成多余帧。但 debug.getinfo 能通过
                // closure 字段获取被调用者的闭包信息。
                // 若调用者已推入相同闭包的条目（如 call_hook 已推入 namewhat="hook"
                // 的条目），或已推入 namewhat 非空的条目（如 call_tm_res 已推入
                // namewhat="metamethod" 的条目），跳过以避免覆盖 namewhat。
                let saved_call_info_len = self.call_info.len();
                let caller_is_lua = self.base > 0
                    && self.base <= self.stack.len()
                    && matches!(&self.stack[self.base - 1], TValue::LClosure(_));
                let already_has_entry = self.call_info.last().map_or(false, |e| {
                    e.closure
                        .as_ref()
                        .map_or(false, |c| Rc::ptr_eq(&c.proto, &proto))
                        || get_closure_from_stack(&self.stack, e)
                            .map_or(false, |c| Rc::ptr_eq(&c.proto, &proto))
                        || (!e.namewhat.is_empty() && !e.is_c)
                });
                let pushed_call_info = !already_has_entry;
                if pushed_call_info {
                    let (caller_base, caller_pc) = if caller_is_lua {
                        (self.base, self.pc)
                    } else {
                        // C 调用者: base=0, saved_pc=0
                        (0usize, 0usize)
                    };
                    self.call_info.push(crate::state::CallInfoEntry {
                        is_c: !caller_is_lua,
                        // move closure 避免 clone + Box 堆分配
                        closure: Some(closure),
                        base: caller_base,
                        saved_pc: caller_pc,
                        name: None,
                        namewhat: "",
                        proto_flag: self.proto_flag,
                        nextraargs: self.nextraargs,
                        is_tailcall: false,
                    });
                } else {
                    // 不需要 closure，drop 它
                    drop(closure);
                }

                self.code = Rc::clone(&proto.code);
                self.constants = Rc::clone(&proto.constants);
                self.protos = proto.protos.clone();
                self.base = func_idx + 1;
                self.pc = 0;
                self.num_params = proto.num_params;
                self.is_vararg = proto.is_vararg();
                self.proto_flag = proto.flag;
                self.nextraargs = 0;
                // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                // upvals 是 Rc<RefCell<Vec>>，这里 clone 出 Vec 供执行期使用
                self.closure_upvals = Rc::clone(&upvals);
                self.tbc_list = None;
                // 不清空 state.exec.open_upval: 全局链表机制下，open_upval 不随函数调用/返回保存/恢复
                // (对应 C 的 L->openupval 全局链表，luaD_precall 不修改它)

                if proto_is_vararg {
                    // vararg 函数: 截断栈到实际参数末尾，VARARGPREP 会处理
                    self.stack.truncate(func_idx + 1 + nargs_actual);
                    for i in nargs_actual..nfixparams {
                        let idx = func_idx + 1 + i;
                        while self.stack.len() <= idx {
                            self.stack.push(TValue::Nil(NilKind::Strict));
                        }
                        self.stack[idx] = TValue::Nil(NilKind::Strict);
                    }
                    // 设置 state.exec.top 供 VARARGPREP 计算 totalargs
                    // (对应 C 的 L->top = ci->func + 1 + nargs)
                    self.top = self.stack.len();
                } else {
                    let frame_end = func_idx + 1 + fsize;
                    while self.stack.len() < frame_end {
                        self.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    for i in nargs_actual..nfixparams {
                        self.stack[func_idx + 1 + i] = TValue::Nil(NilKind::Strict);
                    }
                    // 设置 state.exec.top 为帧末尾 (对应 C 的 L->top = ci->top)
                    self.top = frame_end;
                }

                // Shield 机制: 防止内层 pcall 的 error 被外层 PcallProtection 错误捕获
                // 当顶部 PcallProtection 有 saved_filled=true（即 yield 穿过外层 pcall）时，
                // push 一个 shield（saved_filled=false）。execute_loop 的 error 处理只处理
                // saved_filled=true 的 PcallProtection，遇到 shield 时 break，
                // 使 error 传播到本 state.pcall，而非被外层 PcallProtection 捕获。
                // 典型场景: pcall(foo) yield 后，foo 的 __close error 应被
                // call_close_method 的 state.pcall 捕获，而非 pcall(foo) 的 PcallProtection。
                let need_shield = self
                    .pcall_protection_stack
                    .last()
                    .map_or(false, |t| t.saved_filled);
                if need_shield {
                    self.pcall_protection_stack
                        .push(crate::state::PcallProtection {
                            saved_code: Rc::new(Vec::new()),
                            saved_constants: Rc::new(Vec::new()),
                            saved_protos: Rc::new(Vec::new()),
                            saved_base: 0,
                            saved_pc: 0,
                            saved_num_params: 0,
                            saved_is_vararg: false,
                            saved_proto_flag: 0,
                            saved_nextraargs: 0,
                            saved_closure_upvals: Rc::new(RefCell::new(UpValVec::new())),
                            saved_tbc_list: None,
                            func_idx: 0,
                            nresults: 0,
                            pcall_kind: crate::state::PcallKind::Pcall,
                            saved_filled: false,
                            is_metamethod: false,
                            metamethod_res: 0,
                            saved_call_stack_len: 0,
                            is_close_continuation: false,
                            is_pairs_continuation: false,
                            saved_call_stack: Vec::new(),
                        });
                }

                // 临时保存并清空 call_stack，避免 execute_loop 中的 op_return
                // 错误地 pop 外层调用帧（如 checkerror 的帧）。
                // state.pcall 的 LClosure 分支不 push call_stack（不像 op_call），
                // 所以 call_stack 中的帧是外层调用的，不应被内层函数的 op_return 使用。
                // 对应 C Lua 中 state.pcall 创建新的 CallInfo，与外层 CallInfo 链隔离。
                // 注意: 不清空 call_info，因为 op_return 只在 call_stack.pop() 成功时
                // 才 pop call_info（call_stack 为空时不会 pop），且调用者（如 call_hook）
                // 可能已 push call_info 供 debug.getinfo 使用。
                let saved_call_stack_frames = std::mem::take(&mut self.call_stack);

                let result = VmExecutor::execute_loop(self);

                // 恢复 call_stack（execute_loop 可能修改了它）
                // 非 yield 路径: execute_loop 中的 op_return 应该走 else 分支（call_stack 为空），
                //   不会 pop 帧也不会 push 帧，call_stack 仍为空。
                // yield 路径: execute_loop 返回 Yield，call_stack 可能有残留（被中断的内层调用），
                //   只保留这些内层帧供协程恢复时使用。
                //   外层帧（saved_call_stack_frames）保存到 PcallProtection.saved_call_stack，
                //   由 finish_pcall_return 恢复。不合并到 call_stack: 否则 resume 后内层帧 pop 完
                //   会错误 pop 外层帧，导致 state.exec.base 跳过 pcall 的 func_idx+1。
                //   外层帧持有 Rc 引用（code/constants/protos 等），必须保存以防释放后悬空指针。
                if !matches!(&result, Ok(VmResult::Yield { .. })) {
                    self.call_stack = saved_call_stack_frames;
                } else {
                    // yield: 只保留内层残留帧（被中断的调用），外层帧保存到 PcallProtection
                    let pp_len = self.pcall_protection_stack.len();
                    // 向下查找第一个未填充的 PcallProtection（与下方 saved_filled 更新逻辑一致）
                    let target_idx = if pp_len > 0 {
                        (0..pp_len)
                            .rev()
                            .find(|&i| !self.pcall_protection_stack[i].saved_filled)
                    } else {
                        None
                    };
                    match target_idx {
                        Some(idx) => {
                            self.pcall_protection_stack[idx].saved_call_stack =
                                saved_call_stack_frames;
                            // remaining（内层帧）已在 self.call_stack 中
                        }
                        None => {
                            // fallback: 找不到目标 PcallProtection（嵌套 yield 场景）时，
                            // 合并到 call_stack 以避免丢弃 Rc 引用导致悬空指针
                            let remaining = std::mem::take(&mut self.call_stack);
                            let mut combined = saved_call_stack_frames;
                            combined.extend(remaining);
                            self.call_stack = combined;
                        }
                    }
                }
                // 恢复 n_ccalls (对应 C 的 decnny / longjmp 恢复 ci->nCcalls)。
                // 非 yield 路径下恢复到 pcall 调用前的值:
                //   - 成功路径: op_return 已递减 op_call 的增量,恢复到 saved_n_ccalls 等价于递减 1
                //   - error 路径: op_call 的增量未被 op_return 递减,需整体恢复
                // yield 路径不恢复 (协程恢复时由 ThreadContext 处理)。
                if !matches!(&result, Ok(VmResult::Yield { .. })) {
                    self.n_ccalls = saved_n_ccalls;
                }

                // pop shield: shield 只在 execute_loop 执行期间需要
                // (yield 路径下也需先 pop shield，再更新顶部 PcallProtection)
                if need_shield {
                    self.pcall_protection_stack.pop();
                }

                // yield 不是错误: 暂存 yield 值,不关闭 TBC 变量
                // (yield 时 TBC 变量应保持 open,等协程恢复后正常关闭)
                // 注意: execute_loop 将 VmError::Yield 转换为 Ok(VmResult::Yield)
                let is_yield = matches!(&result, Ok(VmResult::Yield { .. }));
                if is_yield {
                    if let Ok(VmResult::Yield { values }) = &result {
                        self.pending_yield = Some(values.clone());
                    }
                    // yield 时清理 last_error_value,避免残留错误影响
                    self.last_error_value = None;
                    self.last_error_msg.clear();
                    // 更新顶部 PcallProtection 的 saved_* 字段和 saved_filled=true
                    // 对应 C Lua 的 CIST_YPCALL CallInfo 保留:
                    // yield 穿过 pcall 后,pcall 返回,但保护状态保留。
                    // 当 inner_func 后续执行 error/return 时,由 execute_loop 检查并处理。
                    // 跳过已 saved_filled=true 的（嵌套 yield 场景，内层 state.pcall 已更新），
                    // 向下查找第一个未填充的 PcallProtection（对应 C Lua 的多层 CallInfo）
                    let pp_len = self.pcall_protection_stack.len();
                    let target_idx = (0..pp_len)
                        .rev()
                        .find(|&i| !self.pcall_protection_stack[i].saved_filled)
                        .or_else(|| {
                            // 全部已填充: 无可更新目标（不应发生）
                            None
                        });
                    if let Some(idx) = target_idx {
                        let top = &mut self.pcall_protection_stack[idx];
                        top.saved_code = saved_code.clone();
                        top.saved_constants = saved_constants.clone();
                        top.saved_protos = saved_protos.clone();
                        top.saved_base = saved_base;
                        // is_metamethod: saved_pc 不 +1，保留指向被中断的指令（OP_LE/OP_MMBIN），
                        // 以便 op_return continuation 时读取该指令并完成（对应 C 的 luaV_finishOp）。
                        // is_close_continuation: saved_pc 不 +1，保留指向 OP_RETURN/OP_CLOSE，
                        // 以便 op_return continuation 时重新执行该指令（对应 C 的 savedpc--）。
                        // 其他: saved_pc + 1，跳过调用 pcall 的 CALL 指令。
                        top.saved_pc = if top.is_metamethod || top.is_close_continuation {
                            saved_pc
                        } else {
                            saved_pc + 1
                        };
                        top.saved_num_params = saved_num_params;
                        top.saved_is_vararg = saved_is_vararg;
                        top.saved_proto_flag = saved_proto_flag;
                        top.saved_nextraargs = saved_nextraargs;
                        top.saved_closure_upvals = Rc::clone(&saved_closure_upvals);
                        top.saved_tbc_list = saved_tbc_list;
                        top.func_idx = func_idx;
                        // 不覆盖 nresults: PcallProtection.nresults 保存的是
                        // call_pcall/call_xpcall 的 nresults (pcall 调用者期望的返回值数),
                        // 供 finish_pcall_return continuation 使用。
                        top.saved_filled = true;
                    }
                }

                // 错误时关闭 to-be-closed 变量（对应 C 的 luaD_closeprotected -> luaF_close）
                // 必须在恢复 saved 状态之前调用，此时 state.exec.closure_upvals/open_upval 仍是 foo 的
                //
                // 在协程中 pcall 走 CIST_YPCALL 路径（对应 C Lua 的 lua_pcallk + continuation），
                // error 时由 finishpcallk 用 yy=1（可 yield）处理 close。这里模拟该机制：
                // 如果可 yield（n_ny_calls==0 且在协程中），用 yy=1 让 __close 可 yield。
                // close_yield 时设置 close_error_status 保存 error 状态，更新 PcallProtection
                // saved_filled=true，返回 LUA_YIELD。resume 时由 finish_close_continuation
                // 继续关闭剩余 TBC 变量（对应 C Lua 的 finishpcallk -> luaF_close 循环）。
                let mut close_yield: Option<Vec<TValue>> = None;
                let close_err = if is_yield {
                    None
                } else {
                    match &result {
                        Ok(_) => None,
                        Err(e) => {
                            // 从 e 构造错误值（始终更新，避免上次 pcall 残留的 last_error_value 污染）
                            // 对应 C Lua 的 longjmp 恢复：错误值来自当前错误，而非全局状态
                            let err_from_e = match e {
                                VmError::RuntimeErrorValue(val) => val.clone(),
                                VmError::RuntimeError(s) => self.intern_str(s.as_str()),
                                other => self.intern_str(&format!("{}", other)),
                            };
                            self.last_error_value = Some(err_from_e);
                            // 保存错误发生时的 call_info 快照 — 对应 C Lua 中 longjmp 后
                            // CallInfo 链表保持完整的行为。call_xpcall 调用错误处理函数之前
                            // 恢复此快照，让 debug.traceback 能看到错误发生时的帧（如 __close 帧）。
                            // 如果有 __close 帧（call_close_method 出错时保存到 last_close_frame），
                            // 追加到快照末尾，模拟 C Lua 中 CallInfo 节点保留在链表中的行为。
                            let mut snapshot = self.call_info.clone();
                            if let Some(ref close_frame) = self.last_close_frame {
                                snapshot.push(close_frame.clone());
                            }
                            self.last_error_call_info = Some(snapshot);
                            self.last_close_frame = None;
                            // 清理 call_info 中 foo 执行期间的残留条目（对应 C 的 L->ci = old_ci）
                            // 必须在 func::close 之前执行，否则 debug.getinfo(2) 会读到残留条目
                            self.call_info.truncate(saved_call_info_len);
                            let close_level = self.base; // foo 的 base
                                                         // __close 的调用者应是 pcall（C 函数），对应 C 版本中 foo 的 ci
                                                         // 在 error 时被弹出，调用栈上只剩 pcall 的 ci。
                                                         // call_close_method 推入的 __close CallInfoEntry 的 base = state.exec.base
                                                         // = close_level。state.exec.stack[close_level-1] 是 foo 闭包（LClosure），
                                                         // 因为 call_pcall 把 foo 覆盖到 pcall 闭包位置。
                                                         // 临时设为 nil（非 LClosure），让 debug.getinfo(2) 返回 "C"。
                                                         // close 后会 truncate 栈到 func_idx，无需恢复。
                            if close_level > 0 && close_level <= self.stack.len() {
                                self.stack[close_level - 1] = TValue::Nil(NilKind::Strict);
                            }
                            // 在协程中用 yy=1（可 yield），对应 C Lua 的 finishpcallk 用 yy=1
                            let close_yy = if self.n_ny_calls == 0 && self.current_thread.is_some()
                            {
                                1
                            } else {
                                0
                            };
                            match crate::func::close(self, close_level, 1, close_yy) {
                                Ok(()) => {}
                                Err(VmError::Yield(values)) => {
                                    close_yield = Some(values);
                                }
                                Err(_) => {}
                            }
                            // 用 clone() 而非 take()，保留 last_error_value 供上层 close 函数读取错误传播
                            self.last_error_value.clone()
                        }
                    }
                };

                // close_yield: __close yield（pcall error 路径的 close）
                // 类似 is_yield: 不恢复 saved_*（保留 __close 的执行状态），
                // 更新 PcallProtection saved_filled=true（保存 call_pcall 调用者状态），
                // 设置 close_error_status 保存 error 状态，返回 LUA_YIELD。
                // resume 时由 finish_close_continuation 继续关闭剩余 TBC 变量。
                let is_close_yield = close_yield.is_some();
                if is_close_yield {
                    // 设置 close_error_status — 保存 error 状态供 resume 时 finish_close_continuation 使用
                    // 对应 C Lua 的 CIST_RECST 保存的错误状态
                    self.close_error_status = self.last_error_value.clone();
                    // 设置 pending_yield 供 call_pcall 传播
                    self.pending_yield = close_yield;
                    // 更新 PcallProtection saved_filled=true（类似 is_yield 路径）
                    let pp_len = self.pcall_protection_stack.len();
                    let target_idx = (0..pp_len)
                        .rev()
                        .find(|&i| !self.pcall_protection_stack[i].saved_filled);
                    if let Some(idx) = target_idx {
                        let top = &mut self.pcall_protection_stack[idx];
                        top.saved_code = saved_code.clone();
                        top.saved_constants = saved_constants.clone();
                        top.saved_protos = saved_protos.clone();
                        top.saved_base = saved_base;
                        top.saved_pc = saved_pc + 1;
                        top.saved_num_params = saved_num_params;
                        top.saved_is_vararg = saved_is_vararg;
                        top.saved_proto_flag = saved_proto_flag;
                        top.saved_nextraargs = saved_nextraargs;
                        top.saved_closure_upvals = Rc::clone(&saved_closure_upvals);
                        top.saved_tbc_list = saved_tbc_list;
                        top.func_idx = func_idx;
                        top.saved_filled = true;
                    }
                }

                // yield 时: 保留 __close/foo 的执行状态（不恢复 saved 状态，不截断栈）
                // 对应 C Lua 中 yield 通过 longjmp 穿过 pcall，pcall 的状态被销毁，
                // 第二次 resume 直接恢复 __close/foo 的执行。
                // is_close_yield: __close 正在执行，state 是 __close 的状态
                // is_yield: foo 直接 yield，state 是 foo 的状态
                if !is_yield && !is_close_yield {
                    self.code = saved_code;
                    self.constants = saved_constants;
                    self.protos = saved_protos;
                    self.base = saved_base;
                    self.pc = saved_pc;
                    self.num_params = saved_num_params;
                    self.is_vararg = saved_is_vararg;
                    self.proto_flag = saved_proto_flag;
                    self.nextraargs = saved_nextraargs;
                    self.closure_upvals = saved_closure_upvals;
                    self.tbc_list = saved_tbc_list;
                }

                match result {
                    Ok(VmResult::Return {
                        nresults: nret,
                        result_base,
                    }) => {
                        // 对应 C 的 adjustresults: nresults >= 0 时补 nil 到 nresults
                        let expected = if nresults == MULT_RET {
                            nret
                        } else if nresults <= 0 {
                            0
                        } else {
                            nresults as usize
                        };

                        // 从 result_base 位置取结果（可能是 VARARGPREP 调整后的 base）
                        let mut tmp_results = Vec::new();
                        for i in 0..nret {
                            if result_base + i < self.stack.len() {
                                let v = std::mem::take(&mut self.stack[result_base + i]);
                                tmp_results.push(v);
                            } else {
                                tmp_results.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                        self.stack.truncate(func_idx);
                        for i in 0..expected {
                            if i < tmp_results.len() {
                                self.stack.push(std::mem::take(&mut tmp_results[i]));
                            } else {
                                self.stack.push(TValue::Nil(NilKind::Strict));
                            }
                        }
                        if pushed_call_info {
                            self.call_info.pop();
                        }
                        0
                    }
                    Ok(VmResult::Yield { .. }) => {
                        // yield: 保留 foo 的执行状态，不截断栈
                        // yield 值已在 pending_yield 中,调用者检查并传播
                        LUA_YIELD
                    }
                    Ok(_) => {
                        self.stack.truncate(func_idx);
                        if pushed_call_info {
                            self.call_info.pop();
                        }
                        0
                    }
                    Err(_) => {
                        if is_close_yield {
                            // __close yield: 不截断栈（保留 __close 的执行状态）
                            // pending_yield 已设置，调用者检查并传播
                            LUA_YIELD
                        } else {
                            // 将截断的值移到 c_safety_keepalive，延迟到下次 lua_pcallk 时释放。
                            // C 代码（如 lsqlite3 的 db_sql_normal_function）在 lua_pcall 失败后
                            // 可能仍持有 userdata 指针并访问其数据区。C Lua 的栈是数组，
                            // truncate 只移动 top 指针，userdata 在 GC 前保持有效。
                            // Rust 用 Vec<TValue>，truncate 立即 drop，Rc 引用计数为 0 时
                            // userdata 内存被释放，导致悬空指针。此 keepalive 模拟 C Lua 行为。
                            //
                            // 仅对 C 发起的 pcall 暂存：Lua 层调用方不持有裸指针，直接截断，
                            // 否则暂存区会随每次 Lua 层 pcall 失败无界增长（且这些值不再被释放）。
                            if self.stack.len() > func_idx {
                                if self.c_pcall_stash {
                                    let drained = self.stack.split_off(func_idx);
                                    self.c_safety_keepalive.extend(drained);
                                } else {
                                    self.stack.truncate(func_idx);
                                }
                            }
                            // close 后 last_error_value 包含最终错误值（可能是原始错误或 __close 错误）
                            // 保留原始 TValue 类型（如数字 43），pcall 返回 (false, err_value)
                            let err_val = close_err.unwrap_or_else(|| TValue::Nil(NilKind::Strict));
                            // 对字符串错误，使用 build_traceback 添加了 source:line 前缀的
                            // last_error_msg（VM 错误如 "attempt to call" 需要前缀；
                            // error()/assert() 的前缀已在 last_error_value 中，二者一致）
                            let err_val =
                                if matches!(err_val, TValue::LongStr(_) | TValue::ShortStr(_))
                                    && !self.last_error_msg.is_empty()
                                {
                                    let msg = self.last_error_msg.clone();
                                    self.g_mut().intern_str(&msg)
                                } else {
                                    err_val
                                };
                            self.last_error_msg.clear();
                            // 清除 last_error_value，防止残留值污染后续 pcall
                            // (对应 C Lua 的 longjmp 后错误状态不跨 pcall 保留)
                            self.last_error_value = None;
                            self.stack.push(err_val);
                            ERR_RUN
                        }
                    }
                }
            }
            TValue::LCFn(lcf) => Self::pcall_c_function(self, func_idx, nresults, lcf.func),
            TValue::CClosure(cc) => Self::pcall_c_function(self, func_idx, nresults, cc.f),
            TValue::BuiltinFn(bf) => {
                Self::pcall_rust_fn(self, func_idx, nresults, bf.call_target(), bf.name_str())
            }
            TValue::RustClosure(rc) => {
                Self::pcall_rust_fn(self, func_idx, nresults, rc.func, rc.name_str())
            }
            _ => {
                self.stack.truncate(func_idx);
                let ty = self.typename(func_val.ty());
                self.push_string(&format!("attempt to call a {} value", ty));
                ERR_RUN
            }
        }
    }

    /// 从 pcall 调用 C 函数（轻量 C 函数或 C 闭包）。
    ///
    /// 对应 C 的 precallC + luaD_poscall：
    /// 1. 设置 api_func_base = func_idx，确保栈空间，调用 f(L)
    /// 2. 把栈顶 n 个结果移动到 func_idx 位置
    fn pcall_c_function(
        &mut self,
        func_idx: usize,
        nresults: i32,
        f: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32,
    ) -> i32 {
        use std::ffi::c_void;

        // precallC: 设置 api_func_base，确保栈 capacity（不改变 len，避免干扰 lua_gettop）
        let saved_api_base = self.api_func_base;
        self.api_func_base = func_idx;
        self.n_ccalls = self.n_ccalls.saturating_add(1);
        // 外部 C 帧开始：C 代码只经 C API 压栈（不推进 top），GC 根须覆盖到 stack.len()
        self.c_frame_depth = self.c_frame_depth.saturating_add(1);

        // 保存 call_info 长度，用于调用后清理（同 pcall_rust_fn 的处理）。
        // C 函数内部可能推入额外 call_info 条目（如元方法条目），
        // 错误路径下这些条目可能未被弹出。
        let saved_call_info_len = self.call_info.len();

        // 推入 CallInfoEntry — 对应 C 的 luaD_precall 创建新 CallInfo
        // pcall 路径下，调用者是 pcall（内部 C 函数），不是 Lua 函数，
        // caller_proto 延迟计算为 None 表示无法从代码分析函数名，
        // lua_getinfo("n") 返回 NULL，触发 luaL_argerror 调用 pushglobalfuncname 查找全局名。
        self.call_info.push(crate::state::CallInfoEntry {
            is_c: true,
            closure: None,
            base: func_idx + 1,
            saved_pc: 0,
            name: None,
            namewhat: "",
            proto_flag: self.proto_flag,
            nextraargs: self.nextraargs,
            is_tailcall: false,
        });

        // 预留 capacity，但不 push nil（push 会改变 stack.len()，导致 C 函数中
        // lua_gettop 返回值偏移）。C 函数需要空间时通过 lua_checkstack 或
        // lua_pushxxx 自动扩展栈。
        let needed_cap = self.stack.len() + MIN_STACK;
        if self.stack.capacity() < needed_cap {
            self.stack.reserve(MIN_STACK);
        }

        // 清空 pending_error，捕获 C 函数中 lua_error 触发的错误
        // 对应 C 的 longjmp 跨 C 函数抛错到 pcall 的 setjmp
        self.pending_error = None;
        let ptr: *mut LuaState = self as *mut LuaState;

        // 调用 C 函数并捕获错误:
        // - 非 lua_use_longjmp: 用 catch_unwind 捕获 panic (lua_error → panic)
        // - lua_use_longjmp: 用 setjmp/longjmp (panic=abort 下 catch_unwind 不工作)
        // 返回 Ok(n) 表示成功, Err(()) 表示 lua_error (pending_error 已设置)
        let call_result: Result<i32, ()> = {
            #[cfg(not(lua_use_longjmp))]
            {
                // 将 f 转换为 extern "C-unwind" 以允许 panic 跨 C 帧展开回 catch_unwind
                // （C 模块编译时加 -fexceptions，GCC 生成 unwind 表使 Rust panic 能通过）
                let f_unwind: unsafe extern "C-unwind" fn(*mut c_void) -> i32 =
                    unsafe { std::mem::transmute(f) };
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    f_unwind(ptr as *mut c_void)
                })) {
                    Ok(n) => Ok(n),
                    Err(payload) => {
                        if self.pending_error.is_some() {
                            Err(())
                        } else {
                            // 非 lua_error 触发的 panic，re-panic 保持原行为
                            std::panic::resume_unwind(payload);
                        }
                    }
                }
            }
            #[cfg(lua_use_longjmp)]
            {
                // lua_use_longjmp 模式: 用 setjmp/longjmp 替代 catch_unwind
                // jmp_buf 分配在 Rust 栈上，指针压入 error_jmp_bufs 供 do_lua_error 取用
                let mut jmp_buf = JmpBuf::default();
                self.error_jmp_bufs.push(jmp_buf.as_mut_ptr());
                let result = unsafe {
                    lua_rs_pcall_c(
                        Some(f),
                        ptr as *mut c_void,
                        jmp_buf.as_mut_ptr() as *mut c_void,
                    )
                };
                self.error_jmp_bufs.pop();
                if result == -1 {
                    Err(())
                } else {
                    Ok(result)
                }
            }
        };

        // 恢复 api_func_base 和 n_ccalls（无论成功失败）
        self.api_func_base = saved_api_base;
        self.n_ccalls = self.n_ccalls.saturating_sub(1);
        self.c_frame_depth = self.c_frame_depth.saturating_sub(1);

        // 清理 call_info: 截断到调用前的长度（同 pcall_rust_fn 的处理）。
        // yield 路径: 不截断（保留 call_info 供 resume 使用）。
        let is_yield = self.pending_yield.is_some();
        if !is_yield {
            self.call_info.truncate(saved_call_info_len);
        }

        // 检查 C 函数内部是否发生了 yield（通过 lua_pcall → pcall → pending_yield）
        // 对应 C Lua 中 yield 通过 longjmp 跨 C 函数传播。
        // 注意: 不 take pending_yield，留给外层 call_c_function 处理（多层 C 调用传播）
        if self.pending_yield.is_some() {
            // yield: 不处理结果，返回 LUA_YIELD
            return LUA_YIELD;
        }

        // 错误路径：lua_error 触发 (longjmp 或 panic) + pending_error
        if let Err(()) = call_result {
            if let Some(err_val) = self.pending_error.take() {
                self.stack.truncate(func_idx);
                self.stack.push(err_val);
                return ERR_RUN;
            }
            // 不应到达此处: longjmp/panic 必须伴随 pending_error
            unreachable!("C function error without pending_error");
        }

        let n = call_result.unwrap();
        // poscall: 把栈顶 n 个结果移动到 func_idx 位置
        let top = self.stack.len();
        let n = n as usize;
        let first_result = top.saturating_sub(n);

        // 计算期望结果数
        let expected = if nresults == MULT_RET {
            n
        } else if nresults <= 0 {
            0
        } else {
            n.min(nresults as usize)
        };

        // 把结果复制到临时 Vec，避免覆盖问题
        let results: Vec<TValue> = (0..n)
            .map(|i| self.stack[first_result + i].clone())
            .collect();

        // 截断到 func_idx，然后推入 expected 个结果
        self.stack.truncate(func_idx);
        for i in 0..expected {
            if i < results.len() {
                self.stack.push(results[i].clone());
            } else {
                self.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        0
    }

    /// 从 pcall 调用 Rust 原生函数（BuiltinFn 或 RustClosure 的 func）
    ///
    /// 公共逻辑：推入 CallInfoEntry → 调用函数指针 → 弹出 CallInfoEntry → 处理结果
    /// BuiltinFn 和 RustClosure 的调用路径完全相同，区别仅在获取 func/name 的方式。
    fn pcall_rust_fn(
        &mut self,
        func_idx: usize,
        nresults: i32,
        func: crate::objects::BuiltinFnPtr,
        name: &'static str,
    ) -> i32 {
        let nargs = self.stack.len().saturating_sub(func_idx + 1);

        // 保存 call_info 长度，用于调用后清理。
        // 函数指针内部可能推入额外 call_info 条目（如元方法条目），
        // 错误路径下这些条目可能未被弹出（call_tm_res 在错误路径下保留元方法条目）。
        // 使用 truncate 确保正确清理，而非简单的 pop（pop 会弹出错误的条目）。
        let saved_call_info_len = self.call_info.len();

        // 推入 CallInfoEntry，让 debug.getinfo/traceback 能正确看到 C 函数帧
        self.call_info.push(crate::state::CallInfoEntry {
            is_c: true,
            closure: None,
            base: func_idx + 1,
            saved_pc: 0,
            name: Some(name),
            namewhat: "function",
            proto_flag: self.proto_flag,
            nextraargs: self.nextraargs,
            is_tailcall: false,
        });

        // 直接调用函数指针 — 无 tag 派发层
        let dispatch_result: Result<(), crate::execute::VmError> =
            func(self, func_idx, nargs, nresults);

        // 清理 call_info: 截断到调用前的长度，移除 pcall_rust_fn 推入的条目
        // 以及函数指针内部可能残留的额外条目（如元方法条目）。
        // yield 路径: 不截断（保留 call_info 供 resume 使用，对应 C Lua 中
        // yield 通过 longjmp 跳出，CallInfo 保留在链表中）。
        let is_yield = matches!(&dispatch_result, Err(crate::execute::VmError::Yield(_)));
        if !is_yield {
            self.call_info.truncate(saved_call_info_len);
        }

        match dispatch_result {
            Ok(()) => 0,
            // yield: C 函数 (如 coroutine.yield 作为 __close) 传播的 yield
            // 对应 C 中 yield 通过 longjmp 穿过 pcall:
            // 不截断栈,保留被调用函数的执行状态供协程恢复时使用
            Err(crate::execute::VmError::Yield(values)) => {
                self.pending_yield = Some(values);
                self.last_error_value = None;
                self.last_error_msg.clear();
                // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
                // state.exec.code/state.exec.pc 未被修改（仍为 close 调用者的上下文）。
                // 需要更新 close continuation 的 PcallProtection，
                // 以便 resume 时 finish_close_continuation 能正确恢复并重新执行 OP_RETURN/OP_CLOSE。
                let pp_len = self.pcall_protection_stack.len();
                let target_idx = (0..pp_len).rev().find(|&i| {
                    !self.pcall_protection_stack[i].saved_filled
                        && self.pcall_protection_stack[i].is_close_continuation
                });
                if let Some(idx) = target_idx {
                    let top = &mut self.pcall_protection_stack[idx];
                    top.saved_code = self.code.clone();
                    top.saved_constants = self.constants.clone();
                    top.saved_protos = self.protos.clone();
                    top.saved_base = self.base;
                    // is_close_continuation: saved_pc 不 +1，保留指向 OP_RETURN/OP_CLOSE
                    top.saved_pc = self.pc;
                    top.saved_num_params = self.num_params;
                    top.saved_is_vararg = self.is_vararg;
                    top.saved_proto_flag = self.proto_flag;
                    top.saved_nextraargs = self.nextraargs;
                    top.saved_closure_upvals = Rc::clone(&self.closure_upvals);
                    top.saved_tbc_list = self.tbc_list;
                    top.func_idx = func_idx;
                    top.saved_filled = true;
                }
                LUA_YIELD
            }
            Err(crate::execute::VmError::RuntimeError(msg)) => {
                self.stack.truncate(func_idx);
                self.push_string(&msg);
                ERR_RUN
            }
            Err(crate::execute::VmError::RuntimeErrorValue(val)) => {
                // 非字符串错误值（如 error(foo)）：保留原始 TValue 放到栈上，
                // 供 pcall 返回 (false, original_value) 而非 (false, string)
                self.stack.truncate(func_idx);
                self.stack.push(val);
                self.top = self.stack.len();
                ERR_RUN
            }
            Err(e) => {
                self.stack.truncate(func_idx);
                self.push_string(&format!("{}", e));
                ERR_RUN
            }
        }
    }
}

// ============================================================================
// LuaState — 栈/C API/加载/GC 根扫描 API（exec 侧，不变量：GlobalState 方法不得再触 self.exec）
// ============================================================================

impl<'a> LuaState<'a> {
    /// 执行 Lua 字节码 (顶层主函数)
    /// base=0: stack[0] 兼作函数入口和寄存器 0
    pub fn execute(&mut self, proto: &Proto<'a>) -> Result<VmResult<'a>, VmError<'a>> {
        // 进入执行前绑定 l_g（规约 R1）
        self.bind_exec();
        if self.stack.is_empty() {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        let fsize = proto.max_stack_size as usize;
        self.code = proto.code.clone();
        self.constants = proto.constants.clone();
        self.protos = proto.protos.clone();
        self.base = 0;
        self.pc = 0;
        self.num_params = proto.num_params;
        self.is_vararg = proto.is_vararg();
        self.proto_flag = proto.flag;
        self.nextraargs = 0;
        self.closure_upvals = Rc::new(RefCell::new(UpValVec::new()));
        self.tbc_list = None;
        self.open_upval = None;

        while self.stack.len() < fsize {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        VmExecutor::execute_loop(self)
    }

    pub fn gettop(&self) -> usize {
        self.stack.len()
    }

    pub fn settop(&mut self, idx: usize) {
        if idx < self.stack.len() {
            self.stack.truncate(idx);
        } else {
            self.stack.resize(idx, TValue::Nil(NilKind::Strict));
        }
    }

    pub fn pop(&mut self, n: usize) {
        let new_len = self.stack.len().saturating_sub(n);
        self.stack.truncate(new_len);
    }

    /// 对应 C 的 lua_remove：删除指定索引处的元素，上方元素下移。
    pub fn remove(&mut self, idx: isize) {
        let abs = self.abs_index(idx);
        if abs == 0 || abs > self.stack.len() {
            return;
        }
        self.stack.remove(abs - 1);
    }

    /// 对应 C 的 lua_absindex:
    ///   return (idx > 0 || is_pseudo(idx)) ? idx : cast_int(L->top - L->ci->func) + idx + 1
    /// 其中 L->top - L->ci->func 等价于 stack.len()（函数帧内有效元素数）
    pub fn abs_index(&self, idx: isize) -> usize {
        let len = self.stack.len() as isize;
        if idx > 0 {
            idx as usize
        } else {
            let abs = len + idx + 1;
            if abs > 0 {
                abs as usize
            } else {
                0
            }
        }
    }

    pub fn rotate(&mut self, idx: isize, n: isize) {
        let abs = self.abs_index(idx);
        if abs == 0 || abs > self.stack.len() {
            return;
        }
        if n > 0 {
            for _ in 0..n {
                let val = self.stack.pop().unwrap();
                self.stack.insert(abs - 1, val);
            }
        } else {
            let count = (-n) as usize;
            for _ in 0..count {
                let val = self.stack.remove(abs - 1);
                self.stack.push(val);
            }
        }
    }

    pub fn copy(&mut self, from_idx: isize, to_idx: isize) {
        let from = self.abs_index(from_idx);
        if from > 0 && from <= self.stack.len() {
            let val = self.stack[from - 1].clone();
            let to = self.abs_index(to_idx);
            if to > 0 {
                if to > self.stack.len() {
                    self.stack.resize(to, TValue::Nil(NilKind::Strict));
                }
                self.stack[to - 1] = val;
            }
        }
    }

    pub fn push_nil(&mut self) {
        self.stack.push(TValue::Nil(NilKind::Strict));
    }

    pub fn push_boolean(&mut self, b: bool) {
        self.stack.push(TValue::Boolean(b));
    }

    pub fn push_integer(&mut self, n: i64) {
        self.stack.push(TValue::Integer(n));
    }

    pub fn push_float(&mut self, n: f64) {
        self.stack.push(TValue::Float(n));
    }

    pub fn push_string(&mut self, s: &str) {
        let ls = self.intern_str(s);
        self.stack.push(ls);
    }

    pub fn push_lstring(&mut self, s: &[u8]) {
        let slot = self.alloc_slot();
        let ls = crate::strings::new_lstr_bytes(slot, &self.string_table, s);
        self.stack.push(ls);
    }

    pub fn push_value(&mut self, val: TValue<'a>) {
        self.stack.push(val);
    }

    pub fn push_light_userdata(&mut self, p: *mut std::ffi::c_void) {
        self.stack.push(TValue::LightUserData(p));
    }

    pub fn push_lua_value(&mut self, val: &TValue<'a>) {
        self.stack.push(val.clone());
    }

    pub fn obj_at(&self, idx: isize) -> Option<&TValue<'a>> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, idx: isize) -> Option<&mut TValue<'a>> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&mut self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn get_global(&mut self, name: &str) -> LuaType {
        let key = self.intern_str(name);
        match self.globals.get(&key) {
            Some(val) => {
                let ty = val.ty();
                self.stack.push(val.clone());
                ty
            }
            None => {
                self.stack.push(TValue::Nil(NilKind::Strict));
                LuaType::Nil
            }
        }
    }

    pub fn set_global(&mut self, name: &str) {
        let key = self.intern_str(name);
        if let Some(val) = self.stack.pop() {
            self.globals.set(key, val);
        }
    }

    pub fn set_field(&mut self, idx: isize, key_name: &str) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = self.intern_str(key_name);
        if abs > 0 && abs <= self.stack.len() {
            let tbl = &mut self.stack[abs - 1];
            if let TValue::Table(ref mut t) = tbl {
                t.set(key, val);
            }
        }
    }

    pub fn get_field(&mut self, idx: isize, key_name: &str) -> LuaType {
        let abs = self.abs_index(idx);
        let key = self.intern_str(key_name);
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                t.get(&key).unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    pub fn create_table(&mut self, narr: usize, nrec: usize) {
        let t = Table::with_capacity_in(self.alloc_slot(), narr, nrec);
        let gcs = unsafe { self.gc.as_mut() };
        gcs.register_table(&t);
        self.stack.push(TValue::Table(t));
    }

    pub fn new_table(&mut self) {
        let t = Table::new_in(self.alloc_slot());
        let gcs = unsafe { self.gc.as_mut() };
        gcs.register_table(&t);
        self.stack.push(TValue::Table(t));
    }

    pub fn raw_get_i(&mut self, idx: isize, i: i64) -> LuaType {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                let tkey = TValue::Integer(i);
                t.get(&tkey).unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    pub fn raw_set_i(&mut self, idx: isize, i: i64) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        if abs > 0 && abs <= self.stack.len() {
            if let TValue::Table(ref mut t) = self.stack[abs - 1] {
                t.set_int(i, val);
            }
        }
    }

    pub fn raw_get(&mut self, idx: isize) -> LuaType {
        let key = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            let val = if let TValue::Table(ref t) = &self.stack[abs - 1] {
                t.get(&key).unwrap_or(TValue::Nil(NilKind::Strict))
            } else {
                TValue::Nil(NilKind::Strict)
            };
            let ty = val.ty();
            self.stack.push(val);
            ty
        } else {
            self.stack.push(TValue::Nil(NilKind::Strict));
            LuaType::Nil
        }
    }

    pub fn raw_set(&mut self, idx: isize) {
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            if let TValue::Table(ref mut t) = self.stack[abs - 1] {
                t.set(key, val);
            }
        }
    }

    pub fn len(&self, idx: isize) -> usize {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            match &self.stack[abs - 1] {
                TValue::Table(t) => t.len() as usize,
                s @ (TValue::LongStr(_) | TValue::ShortStr(_)) => lua_string_len(s),
                TValue::Integer(_) | TValue::Float(_) => 0,
                _ => 0,
            }
        } else {
            0
        }
    }

    pub fn check_stack(&mut self, extra: usize) -> bool {
        let needed = self.stack.len() + extra;
        if needed > self.stack.capacity() {
            self.stack.reserve(extra);
        }
        true
    }

    /// 构建堆栈回溯字符串 — 对应 C 的 luaL_traceback
    ///
    /// 格式:
    /// ```text
    /// msg
    /// stack traceback:
    ///         [C]: in global 'assert'
    ///         (command line):1: in main chunk
    ///         [C]: in ?
    /// ```
    pub fn traceback_string(&self, msg: &str, _level: usize) -> String {
        let mut result = String::new();
        if !msg.is_empty() {
            result.push_str(msg);
            result.push('\n');
        }
        result.push_str("stack traceback:");
        // 从 call_info 构建回溯
        if self.call_info.is_empty() {
            // 没有调用信息，使用简化的回溯
            result.push_str("\n\t[C]: in ?");
        } else {
            for entry in &self.call_info {
                let (src, line, name, _) = crate::execute::compute_caller_info(&self.stack, entry);
                result.push('\n');
                result.push('\t');
                if entry.is_c {
                    result.push_str("[C]: in ");
                    result.push_str(&name);
                } else {
                    if line > 0 {
                        result.push_str(&format!("{}:{}: in ", src, line));
                    } else {
                        result.push_str(&format!("{}: in ", src));
                    }
                    result.push_str(&name);
                }
            }
            // 最后添加 [C]: in ?
            result.push_str("\n\t[C]: in ?");
        }
        result
    }

    /// 推入调用栈条目 — 用于堆栈回溯
    pub fn push_call_info(&mut self, entry: CallInfoEntry<'a>) {
        self.call_info.push(entry);
    }

    /// 弹出调用栈条目
    pub fn pop_call_info(&mut self) {
        self.call_info.pop();
    }

    pub fn load_buffer(&mut self, code: &str, chunk_name: &str) -> i32 {
        // 使用阈值触发 GC 而非强制完整 GC — 避免每个 load() 调用都做 O(objects) 标记。
        // 当 gc_estimate 超过 collect_threshold 时自动收集，否则跳过。
        // 这对 construct.lua（206,780 次 load() 调用）是关键的优化：
        // 非强制 GC 路径节省了完整标记遍历的 all-objects 扫描开销。
        self.maybe_collect_gc();
        match crate::compiler::compile(self, code, chunk_name) {
            Ok(proto) => {
                let nup = proto.size_upvalues as usize;
                let upvals = Rc::new(RefCell::new(UpValVec::new()));
                upvals
                    .borrow_mut()
                    .push(Rc::new(RefCell::new(UpVal::Closed {
                        value: TValue::Table(self.globals.clone()),
                    })));
                for _ in 1..nup {
                    upvals
                        .borrow_mut()
                        .push(Rc::new(RefCell::new(UpVal::Closed {
                            value: TValue::Nil(NilKind::Strict),
                        })));
                }
                let closure = MyRc::<LClosure, SlotAlloc>::new_in(
                    self.alloc_slot(),
                    LClosure {
                        gc_header: GCObjectHeader::new(),
                        proto: Rc::new(proto),
                        upvals,
                    },
                );
                {
                    let id = unsafe {
                        self.gc.as_mut().register_object(
                            std::mem::size_of::<LClosure>() + /* upvals 尺寸估算 */ 0,
                            WeakValue::LClosure(closure.downgrade()),
                        )
                    };
                    closure.gc_header.set_id(id);
                }
                self.stack.push(TValue::LClosure(closure));
                0
            }
            Err(err_msg) => {
                self.push_string(&err_msg);
                ERR_SYNTAX
            }
        }
    }

    /// 从已读取的字节数组加载 Lua 代码。处理 BOM、shebang、编码与二进制签名。
    fn load_bytes(&mut self, bytes: &mut [u8], chunk_name: &str, mode: Option<&str>) -> i32 {
        let after_bom = skip_bom_mut(bytes);
        let (skipped_comment, first, rest, rest_start) = skip_comment(after_bom);

        let is_binary = first == Some(LUA_SIGNATURE[0]);

        if is_binary && !mode_allows_binary(mode) {
            self.push_string("attempt to load a binary chunk (mode is 'text')");
            return ERR_SYNTAX;
        }
        if !is_binary && !mode_allows_text(mode) {
            self.push_string("attempt to load a text chunk (mode is 'binary')");
            return ERR_SYNTAX;
        }
        if is_binary {
            // 二进制 chunk: 使用 undump_to_proto 解析 (对应 C 的 luaU_undump)
            // 传入 rest (跳过 BOM 和注释后的部分，从 LUA_SIGNATURE 开始)
            let slot = self.alloc_slot();
            return match crate::compiler::bytecode_dump::undump_to_proto(
                rest,
                slot,
                &self.string_table,
            ) {
                Ok(mut proto) => {
                    // 驻留化字符串 (LongString → ShortString)
                    crate::stdlib::base_lib::intern_proto_strings(&mut proto, self);
                    let nup = proto.size_upvalues as usize;
                    let upvals = Rc::new(RefCell::new(UpValVec::new()));
                    upvals
                        .borrow_mut()
                        .push(Rc::new(RefCell::new(UpVal::Closed {
                            value: TValue::Table(self.globals.clone()),
                        })));
                    for _ in 1..nup {
                        upvals
                            .borrow_mut()
                            .push(Rc::new(RefCell::new(UpVal::Closed {
                                value: TValue::Nil(NilKind::Strict),
                            })));
                    }
                    let closure = MyRc::<LClosure, SlotAlloc>::new_in(
                        self.alloc_slot(),
                        LClosure {
                            gc_header: GCObjectHeader::new(),
                            proto: Rc::new(proto),
                            upvals,
                        },
                    );
                    self.stack.push(TValue::LClosure(closure));
                    0
                }
                Err(e) => {
                    self.push_string(&format!("bad binary chunk: {}", e));
                    ERR_SYNTAX
                }
            };
        }
        let rest = if skipped_comment {
            if let Some(rest_start) = rest_start {
                after_bom[rest_start - 1] = b'\n';
                &after_bom[(rest_start - 1)..]
            } else {
                rest
            }
        } else {
            rest
        };
        let source = decode_source_bytes(&rest);
        self.load_buffer(&source, chunk_name)
    }

    pub fn close_state(&mut self) {
        if self.gc_closing {
            return;
        }
        // 关闭同样是「执行入口」：下面要跑 `__close` / finalizer（可能回到 Lua 代码），
        // 收尾的完整回收还要读 `G(L)->main`，按规约 R1 必须先重绑 `l_g` / `G.main`。
        // 经 `Drop for LuaState` 进来的调用方已经绑过，但 `cli::main` 的显式
        // `close_state()` 之前没有 —— 那时 `G.main` 仍指向 `new_main*` 里已返回的
        // 局部栈帧（`LuaState` 被移动后旧地址失效），收尾 GC 会栈越界
        // （ASan: stack-use-after-return，`lua -v` 无脚本路径必崩）。
        self.bind_exec();
        self.g_mut().gc_closing = true;
        let _ = crate::func::close(self, 1, 0, 0);
        let mut to_finalize: Vec<TValue> = Vec::new();
        for t in self.finobj_list.borrow_mut().drain(..).rev() {
            to_finalize.push(TValue::Table(t));
        }
        for u in self.ud_finobj_list.borrow_mut().drain(..).rev() {
            to_finalize.push(TValue::UserData(u));
        }
        self.call_finalizers(to_finalize);
        // 断环必须发生在最后一次完整回收**之前**：此时 globals / registry / package.loaded
        // 仍是 GC 根，环内对象（如 `M.rrotate` 上值捕获模块表 M、metatable ↔ __gc 闭包）
        // 会被判为可达而逃过清扫；若等 Drop for GlobalState 再断，外部引用释放后已无
        // 第二次回收，纯 Rc 环永久存活（ASan 报 indirect leak）。
        self.g().break_close_refs();
        // 关闭前做一次完整回收：`func::close` 关上值、finalizer 运行都可能**新造出**
        // 不可达的 Rc 自引用环（闭包↔上值、协程↔执行态、表自引用），这些环只能靠
        // GC 打断（sweep 清空死对象的引用），之后就交给 Rc 归零释放。
        // 不做这一步，这类环会一直留到进程退出（LSan 报泄漏）。
        // 对应 C 的 lua_close → luaC_freeallobjects 的「关闭即释放全部」语义。
        self.collect_gc();
        if let Some(code) = self.exit_requested {
            std::process::exit(code);
        }
    }

    pub fn traceback(&mut self, msg: &str, _level: usize) {
        let trace = format!("stack traceback:\n\t...\n{}", msg);
        self.push_string(&trace);
    }

    pub fn push_rust_fn(&mut self, _f: fn(&mut GlobalState) -> i32, tag: usize) {
        self.push_light_userdata(tag as *mut std::ffi::c_void);
    }

    pub fn load_filex(&mut self, filename: Option<&str>, mode: Option<&str>) -> i32 {
        let chunk_name = filename
            .map(|f| format!("@{}", f))
            .unwrap_or_else(|| "=stdin".to_string());

        let mut bytes = match filename {
            Some(name) => match std::fs::read(name) {
                Ok(b) => b,
                Err(err) => {
                    self.push_fstring(&format!("cannot open {}: {}", name, err));
                    return ERR_FILE;
                }
            },
            None => {
                let mut buf = Vec::new();
                if let Err(err) = std::io::stdin().read_to_end(&mut buf) {
                    self.push_fstring(&format!("cannot read stdin: {}", err));
                    return ERR_FILE;
                }
                buf
            }
        };

        self.load_bytes(&mut bytes, &chunk_name, mode)
    }
}

impl<'a> LuaState<'a> {
    pub fn push_fstring(&mut self, fmt: &str) {
        self.push_string(fmt);
    }

    pub fn get_type(&self, idx: isize) -> LuaType {
        self.obj_at(idx).map(|v| v.ty()).unwrap_or(LuaType::Nil)
    }

    pub fn to_integer(&self, idx: isize) -> Option<i64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i),
            Some(TValue::Float(f)) => crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq),
            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                lua_string_as_str(s).parse::<i64>().ok()
            }
            _ => None,
        }
    }

    pub fn to_number(&self, idx: isize) -> Option<f64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i as f64),
            Some(TValue::Float(f)) => Some(*f),
            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                crate::float_utils::f64_from_str(lua_string_as_str(s))
            }
            _ => None,
        }
    }

    pub fn to_boolean(&self, idx: isize) -> bool {
        !matches!(
            self.obj_at(idx),
            Some(TValue::Nil(_)) | Some(TValue::Boolean(false))
        )
    }

    pub fn to_string(&self, idx: isize) -> Option<String> {
        match self.obj_at(idx) {
            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                Some(lua_string_as_str(s).to_string())
            }
            Some(TValue::Integer(i)) => Some(i.to_string()),
            Some(TValue::Float(f)) => Some(format_float(*f)),
            _ => None,
        }
    }

    pub fn to_lstring(&self, idx: isize) -> Option<(String, usize)> {
        match self.obj_at(idx) {
            Some(s @ (TValue::LongStr(_) | TValue::ShortStr(_))) => {
                let text = lua_string_as_str(s).to_string();
                let len = lua_string_len(s);
                Some((text, len))
            }
            _ => None,
        }
    }

    pub fn to_userdata(&self, idx: isize) -> *mut std::ffi::c_void {
        match self.obj_at(idx) {
            Some(TValue::LightUserData(p)) => *p,
            _ => std::ptr::null_mut(),
        }
    }

    pub fn typename_at(&self, idx: isize) -> &'static str {
        self.typename(self.get_type(idx))
    }

    pub fn load_file(&mut self, fname: Option<&str>) -> i32 {
        self.load_filex(fname, None)
    }
}

// ============================================================================
// GlobalState — 共享(跨协程)全局字段（= C 的 global_State）
// ============================================================================
//
// 【不变量 · 务必遵守】GlobalState 的方法（含私有 helper）**不得（直接或传递地）
// 访问调用者的 LuaState**。原因：执行路径上调用者持有 `&mut LuaState`（= 某个
// 线程 exec 的 Box 内容），而 GlobalState 方法再取同一对象的 `&mut`，就构成同一
// 分配上的两个 live `&mut` ——debug 下看不出，release 会被优化成错误行为
// （实测：table.sort 错误值丢失、pcall 错误前缀丢失）。
// 因此所有 exec 侧 API 都定义在 `impl LuaState`；GlobalState 侧只保留：
//   1) 生命周期与全局态：new_shared/from_proto/Drop、intern_*/gc_*/register_*
// 新增 exec 侧方法时请加到 LuaState。

impl<'a> std::fmt::Debug for GlobalState<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 手写：`io` 是 `&mut dyn Io`（无 Debug），且共享块字段量大，只打印摘要。
        f.debug_struct("GlobalState")
            .field("main", &self.main)
            .field("globals", &self.globals)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

pub struct GlobalState<'a> {
    // === 主线程登记（对应 C 的 G.mainthread）===
    /// 主线程执行态指针 —— 对应 C 的 `global_State.mainthread`。
    /// 本设计中主线程的 `LuaState` 由调用方持有（`LuaState::owner` 持有本结构），
    /// 此处仅登记其指针：GC 需要把主线程栈当根（协程运行时主线程挂起但栈仍存活），
    /// 由主线程在每个进入点 `bind_exec()` 重绑定（规约 R1：移动后旧指针失效）。
    pub main: *mut LuaState<'a>,
    /// 回边中断检查摊销计数器 — 仅回跳指令 (JMP/FORLOOP/条件跳回) 递增,
    /// 64 次边界时原子加载 INTERRUPTED。放 state 而非循环局部: 借用安全
    /// (Cell 通过 &GlobalState 可写), 跳转 handler 内联增量免 &mut 引用穿透
    /// (CI #90 实证引用参数把增量编译成每次回跳的内存往返, 整数 bench +27%)。
    /// 值不参与语义, 重置/不重置均只影响中断延迟最多 63 次回跳。
    pub tick: std::cell::Cell<u64>,
    pub trap: bool,
    pub twups_linked: bool,
    pub is_in_twups: bool,

    // 公用字段
    pub gc: NonNull<GCState<'a>>,

    // 高层 API 字段（原 GlobalState）
    pub globals: Table<'a>,
    pub registry: Table<'a>,
    /// 字符串表 — 通过 Rc 共享以支持 lua_newthread 创建的 thread 共享主线程的字符串表
    /// （对应 C Lua 中 global_State 的 strt 字段，所有 thread 共享同一份 string table）
    pub string_table: Rc<StringTable>,
    /// 预 intern 的元方法名数组 — 对应 C 的 `G(L)->tmname[TM_N]`。
    /// 在 `GlobalState::new` 时用 `string_table.intern` 初始化一次，
    /// 后续 `make_tm_tvalue` 直接 `clone()` 复用，保证 ptr_eq 快速路径。
    /// 通过 Rc 共享以支持 lua_newthread 创建的 thread 共享主线程的 tmnames
    /// （保证不同 thread 间元方法名 ptr_eq 一致，元表查找快速路径不被破坏）
    pub tmnames: Rc<Box<[TValue<'a>; TM_N]>>,

    // C API 导出层使用：当前 C 函数帧的 func 位置（0-based 栈索引）。
    // C API 的正索引相对于此位置；Lua 代码路径不使用此字段。
    // 0 表示栈底（主线程初始状态）。
    pub dmt: DefaultMetatables<'a>,
    /// io.output 设置的当前输出流 — None 表示使用 stdout
    /// 对应 C liolib 中存储在 registry[IO_OUTPUT] 的默认输出文件句柄
    /// Rc 共享：默认输出流是 state 级共享状态，所有 thread 共享
    pub io_output: Rc<RefCell<Option<Box<dyn Write>>>>,
    /// 文件句柄注册表 — key 是 UserData 的 gc_header.ptr_id，value 是 FILE* 指针
    /// 对应 C 的 luaL_Stream 中存储的 FILE*。UserData 本身不存数据，通过此 map 关联。
    /// Rc 共享：所有 thread 共享同一份句柄表（对应 C 中 FILE* 存于 userdata/registry）
    pub file_handles:
        Rc<RefCell<std::collections::HashMap<u32, TValue<'a>, crate::objects::FxBuildHasher>>>,
    /// 标记哪些文件句柄是 io.popen 创建的（关闭时用 pclose 而非 fclose）
    /// 对应 C 的 LStream.closef = &io_pclose
    /// Rc 共享：同 file_handles
    pub popen_handles: Rc<RefCell<std::collections::HashSet<u32, crate::objects::FxBuildHasher>>>,
    /// 当前默认输入流的 UserData ptr_id — None 表示使用 io.stdin
    /// 对应 C 的 registry[IO_INPUT]
    /// Rc 共享：默认输入流是 state 级共享状态
    pub io_input_handle: Rc<std::cell::Cell<Option<u32>>>,
    /// 当前默认输出流的 UserData ptr_id — None 表示使用 io.stdout
    /// 对应 C 的 registry[IO_OUTPUT]
    /// Rc 共享：默认输出流是 state 级共享状态
    pub io_output_handle: Rc<std::cell::Cell<Option<u32>>>,
    pub global_state: Rc<RefCell<GlobalShared<'a>>>,
    pub ci: Option<Box<CallInfo>>,
    /// 当前正在调用的 C 函数名（用于 traceback）— None 表示不在 C 函数中
    pub last_c_function: Option<String>,
    /// 数学库随机数生成器状态 — 对应 C 的 RanState (math.random/randomseed)
    /// Rc 共享：随机状态属于 C global_State 的 seed，所有 thread 共享同一序列
    pub math_random_state: Rc<RefCell<Option<Box<crate::stdlib::math_lib::RandState>>>>,
    /// warning 系统是否开启 — 对应 C 的 G(L)->warnf == warnfon
    /// Rc 共享：warn 状态属于 C global_State，所有 thread 共享同一份
    pub warn_on: Rc<std::cell::Cell<bool>>,
    /// warning 是否有未完成的消息（tocont=true 后等待下一段）— 对应 C 的 warnfcont 状态
    /// Rc 共享：同上，对应 C global_State 的 warnf 状态
    pub warn_pending: Rc<std::cell::Cell<bool>>,
    /// 主线程对象（用于 coroutine.running() 在主线程返回 thread）
    /// MyRc 共享：allocator 路由块（`RcPayload::HAS_ALLOC_SLOT`），构造期在 owner
    /// Box 之前，沿用 globals/registry/MEMERRMSG 先例传 null 槽 → std 分配。
    pub main_thread: MyRc<LuaThread<'a>, SlotAlloc>,
    /// 弱引用表列表 — setmetatable 设置 __mode 时注册，collectgarbage 时清理
    /// 使用 Weak 引用避免阻止表本身的回收
    pub weak_tables: Rc<RefCell<Vec<crate::objects::TableDataWeak<'a>>>>,
    /// op_concat 的 GC 计数器 — 字符串不注册到 GC metas，用计数器限制
    /// 每 concat_gc_interval 次 op_concat 触发一次 GC（清理弱引用表等）
    pub concat_gc_counter: std::cell::Cell<usize>,
    pub concat_gc_interval: std::cell::Cell<usize>,
    /// 有 __gc 元方法的对象列表 — setmetatable 设置 __gc 时注册
    /// GC 时检查不可达的对象，调用其 finalizer 后再释放
    pub finobj_list: Rc<RefCell<Vec<Table<'a>>>>,
    /// 有 __gc 元方法的 UserData 列表 — FILE* 等通过默认元表设置的 userdata
    /// GC 时检查不可达的 UserData，调用其 finalizer（fclose）后释放
    /// 存储 Rc<Udata> 而非 Udata 深拷贝：确保 __gc 在原始 userdata 上调用，
    /// 这样 C 模块（如 lsqlite3）能通过 light userdata 指针正确清理 registry 引用
    pub ud_finobj_list: Rc<RefCell<Vec<MyRc<crate::objects::Udata<'a>, SlotAlloc>>>>,
    /// 状态正在关闭 — 对应 C 的 g->gcstp & GCSTPCLS
    /// true 时不再注册新的 finalizer 对象（close 中创建的对象不会被 finalize）
    pub gc_closing: bool,
    /// os.exit 请求的退出码 — 在 finalizer 中调用 os.exit(code, true) 时设置，
    /// close_state 处理完所有 finalizer 后据此退出进程。
    pub exit_requested: Option<i32>,
    /// GC 标记阶段缓存的 "__mode" 字符串键 — 避免每次 mark_tvalue 都 intern_str
    /// （perf: mark_tvalue 中 __mode 查找占 ~2%）
    pub cached_mode_key: std::cell::RefCell<Option<TValue<'a>>>,
    /// GC finalizer 阶段缓存的 "__gc" 字符串键 — 避免每次 collect_finalizers/call_finalizers 都 intern_str
    pub cached_gc_key: std::cell::RefCell<Option<TValue<'a>>>,
    /// 上次 GC 后的 gc_estimate 快照 — 用于判断 malloc_trim 是否值得调用
    /// （仅当释放了大量内存时才 trim，避免每次 GC 都做系统调用）
    pub last_gc_estimate: usize,

    /// 内存管理器 — lmem 层（mem.rs），C 风格分配原语经 allocator 动态分发执行。
    /// Rust API（`LuaState::new_main_with_allocator`）注入自定义 Allocator 或
    /// DefaultAllocator；C API（lua_newstate / lua_setallocf）注入 CapiAllocator
    /// 包装宿主的 lua_Alloc（lua_getallocf 经 allocf_parts 取回，skynet 用 ud 存
    /// snlua 指针）。不归 GC 管理（外部 C 代码所有），Rust 仅持有裸指针。
    pub mem: MemState<'a, AllocOwner>,

    /// 输入输出缓冲区 — 用于模拟标准输入输出
    pub io: &'a mut dyn crate::mock::io_mock::Io,
}

// C 辅助函数: setjmp/longjmp 包装 (capi_variadic.c)
// 用于 lua_use_longjmp 模式下替代 catch_unwind (对应 C 的 LUA_USE_LONGJMP)。
// 启用条件: size_optimized (panic=abort) 或显式 --features lua_longjmp。
#[cfg(lua_use_longjmp)]
extern "C" {
    pub(crate) fn lua_rs_pcall_c(
        f: Option<unsafe extern "C" fn(*mut std::ffi::c_void) -> i32>,
        L: *mut std::ffi::c_void,
        buf: *mut std::ffi::c_void,
    ) -> i32;
    pub(crate) fn lua_rs_longjmp(buf: *mut std::ffi::c_void) -> !;
}

/// 调用栈条目 — 用于堆栈回溯和 debug.getinfo
#[derive(Debug, Clone)]
pub struct CallInfoEntry<'a> {
    pub is_c: bool,
    /// Lua 函数引用（C 函数为 None）
    pub closure: Option<MyRc<crate::objects::LClosure<'a>, SlotAlloc>>,
    /// 栈帧基址（对应 C 的 ci->func + 1）
    /// Lua 函数帧: caller's base (state.exec.base before op_call updates it)
    /// C 函数帧: callee's base (func_idx + 1)
    /// perf: 用于从 state.exec.stack[base - 1] 延迟计算 caller_proto, 避免 op_call 中 Rc::clone
    pub base: usize,
    /// 调用时的 PC（用于计算 currentline）
    pub saved_pc: usize,
    /// 函数名（C 函数帧/hook/metamethod 预设置；Lua 函数帧默认 None，traceback 时实时计算）
    /// perf: 用 Option<&'static str> 替代 String, 避免 op_call 热路径中 String::new 的 24 字节开销
    pub name: Option<&'static str>,
    /// 函数名类型: "local", "global", "method", "field", "hook", "metamethod", ""
    /// （C 函数帧/hook/metamethod 预设置；Lua 函数帧默认空，traceback 时实时计算）
    /// perf: 用 &'static str 替代 String, 所有值均为静态字符串
    pub namewhat: &'static str,
    /// 调用者的 proto_flag（PF_VAHID / PF_VATAB）— 用于 debug.getlocal level > 1
    pub proto_flag: u8,
    /// 调用者的 nextraargs — 用于 debug.getlocal level > 1 的 vararg 访问
    pub nextraargs: i32,
    /// 是否为尾调用（对应 C 的 CIST_TAIL）— debug.getinfo(1).istailcall
    pub is_tailcall: bool,
}

/// 从栈切片延迟计算 closure 引用 — 核心逻辑
///
/// 对于 Lua 函数帧: closure = stack[entry.base - 1] (若为 LClosure)
/// 对于 C 函数帧: 返回 None
/// 对于 base=0 或 stack 越界: 返回 None
///
/// perf: 替代 CallInfoEntry.closure 字段, 避免 op_call 中 Rc::clone
pub fn get_closure_from_stack<'a, 'b>(
    stack: &'b [TValue<'a>],
    entry: &CallInfoEntry<'a>,
) -> Option<&'b MyRc<LClosure<'a>, SlotAlloc>> {
    if entry.is_c {
        return None;
    }
    if entry.base > 0 && entry.base <= stack.len() {
        if let TValue::LClosure(c) = &stack[entry.base - 1] {
            return Some(c);
        }
    }
    None
}

/// 从 CallInfoEntry 延迟计算 closure 引用 — 避免 op_call 热路径中 Rc::clone
///
/// 优先返回 entry.closure (若已设置, 如 hook/metamethod 路径);
/// 否则从 state.exec.stack[entry.base - 1] 延迟计算 (op_call 快速路径)
pub fn get_closure_for_ci<'a, 'b>(
    state: &'b LuaState<'a>,
    ci_idx: usize,
) -> Option<&'b MyRc<LClosure<'a>, SlotAlloc>> {
    let entry = &state.call_info[ci_idx];
    if entry.closure.is_some() {
        return entry.closure.as_ref();
    }
    get_closure_from_stack(&state.stack, entry)
}

/// 从栈切片延迟计算 caller_proto 引用 — 核心逻辑
///
/// 对于 Lua 函数帧: caller_proto = stack[entry.base - 1].proto (若为 LClosure)
/// 对于 C 函数帧: 返回 None
/// 对于 base=0 或 stack 越界: 返回 None
pub fn get_caller_proto_from_stack<'a, 'b>(
    stack: &'b [TValue<'a>],
    entry: &CallInfoEntry<'a>,
) -> Option<&'b Rc<Proto<'a>>> {
    if entry.is_c {
        return None;
    }
    if entry.base > 0 && entry.base <= stack.len() {
        if let TValue::LClosure(c) = &stack[entry.base - 1] {
            return Some(&c.proto);
        }
    }
    None
}

/// 从 CallInfoEntry 延迟计算 caller_proto 引用 — 避免在 op_call 热路径中 Rc::clone
///
/// 对于 Lua 函数帧: caller_proto = state.exec.stack[entry.base - 1].proto (若为 LClosure)
/// 对于 C 函数帧: 返回 None (由 get_caller_proto_for_ci 处理 C 帧的特殊路径)
/// 对于 base=0 或 stack 越界: 返回 None
///
/// perf: op_call 中移除 1 次 Rc::clone (atomic inc) + CallInfoEntry drop 时 1 次 Rc dec
pub fn get_caller_proto_ref<'a, 'b>(
    state: &'b LuaState<'a>,
    entry: &CallInfoEntry<'a>,
) -> Option<&'b Rc<Proto<'a>>> {
    get_caller_proto_from_stack(&state.stack, entry)
}

/// 从 call_info 索引延迟计算 caller_proto 引用 (用于 lua_getinfo / traceback)
///
/// 对于 Lua 函数帧: 从 state.exec.stack[entry.base - 1] 获取 (同 get_caller_proto_ref)
/// 对于 C 函数帧 (无预设置 name, 即 call_c_function 外部 C 函数):
///   从前一个 call_info 条目的 closure 获取 (对应原 call_c_function 的计算逻辑)
/// 对于 C 函数帧 (有预设置 name, 如 BuiltinFn/RustClosure): 返回 None
///
/// perf: 替代 CallInfoEntry.caller_proto 字段, 避免 op_call/call_c_function 中 Rc::clone
pub fn get_caller_proto_for_ci<'a, 'b>(
    state: &'a LuaState<'b>,
    ci_idx: usize,
) -> Option<&'a Rc<Proto<'b>>> {
    let entry = &state.call_info[ci_idx];
    if !entry.is_c {
        return get_caller_proto_ref(state, entry);
    }
    // C frame: 仅 call_c_function (name=None) 需要 caller_proto 用于 lua_getinfo('n')
    if entry.name.is_some() {
        return None;
    }
    if ci_idx > 0 {
        if let Some(c) = get_closure_for_ci(state, ci_idx - 1) {
            return Some(&c.proto);
        }
    }
    None
}

// ============================================================================
// 构造
// ============================================================================

impl<'a> GlobalState<'a> {
    /// 对应 C 的 lua_newstate → global_State 部分（模块私有：只能由
    /// `LuaState::new_main_with_allocator` 调用，其他模块无法构造 `GlobalState`）。
    fn new_shared(io: &'a mut dyn crate::mock::io_mock::Io, allocator: Box<dyn Allocator>) -> Self {
        // 分配器宿主先建立：槽自此刻起地址稳定且非 null，供本函数内所有早期
        // 对象（memerrmsg / globals / registry / main_thread）使用——
        // 这消除了「构造期尚无槽 → 传 null → 退回 std::alloc」的旧兜底。
        // 宿主为共享句柄：`GlobalShared` 持一份（GC 存储与其释放宿主同生共死），
        // `mem` 持一份（lmem 路由），二者指向同一内层分配器。
        let alloc = crate::mem::AllocOwner::new(allocator);
        let alloc_slot = SlotAlloc::from_owner(&alloc);
        let string_table = Rc::new(StringTable::new());
        let global_state: Rc<RefCell<GlobalShared<'a>>> = Rc::new(RefCell::new(GlobalShared::new(
            alloc.clone(),
            &string_table,
        )));
        let gs = &mut global_state.borrow_mut();
        let globals = {
            let t: Table<'a> = Table::new_in(alloc_slot);
            gs.gc.register_table(&t);
            t
        };
        let registry = {
            let t: Table<'a> = Table::new_in(alloc_slot);
            gs.gc.register_table(&t);
            t
        };
        registry.set(TValue::Integer(2), TValue::Table(globals.clone()));

        let tmnames = Rc::new(init_tmnames(&string_table));
        GlobalState {
            main: std::ptr::null_mut(),
            trap: false,
            tick: std::cell::Cell::new(0),
            twups_linked: false,
            is_in_twups: false,
            gc: NonNull::new(&mut gs.gc).unwrap(),
            globals,
            registry,
            string_table,
            tmnames,
            dmt: DefaultMetatables::new(),
            io_output: Rc::new(RefCell::new(None)),
            file_handles: Rc::new(RefCell::new(std::collections::HashMap::with_hasher(
                crate::objects::FxBuildHasher::default(),
            ))),
            popen_handles: Rc::new(RefCell::new(std::collections::HashSet::with_hasher(
                crate::objects::FxBuildHasher::default(),
            ))),
            io_input_handle: Rc::new(std::cell::Cell::new(None)),
            io_output_handle: Rc::new(std::cell::Cell::new(None)),
            global_state: global_state.clone(),
            ci: None,
            last_c_function: None,
            math_random_state: Rc::new(RefCell::new(None)),
            warn_on: Rc::new(std::cell::Cell::new(true)),
            warn_pending: Rc::new(std::cell::Cell::new(false)),
            main_thread: MyRc::new_in(
                alloc_slot,
                LuaThread {
                    function: None,
                    is_main: true,
                    context: Rc::new(RefCell::new(ThreadContext::default())),
                    c_state: std::cell::Cell::new(std::ptr::null_mut()),
                },
            ),
            weak_tables: Rc::new(RefCell::new(Vec::new())),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(32768),
            finobj_list: Rc::new(RefCell::new(Vec::new())),
            ud_finobj_list: Rc::new(RefCell::new(Vec::new())),
            gc_closing: false,
            exit_requested: None,
            cached_mode_key: std::cell::RefCell::new(None),
            cached_gc_key: std::cell::RefCell::new(None),
            last_gc_estimate: 0,
            mem: MemState::new_complete(alloc),
            io: io,
        }
    }

    /// 初始化栈: 对应 C 的 stack_init
    /// 分配 BASIC_STACK_SIZE + EXTRA_STACK 容量，推入函数入口 nil
    /// stack[0] = nil (函数入口, ci->func)
    /// top = stack + 1 (1 个元素在用)
    /// pub(crate)：capi 的 lua_newthread 需要为新线程做同样的 stack_init。
    pub(crate) fn init_stack() -> Vec<TValue<'a>> {
        let mut stack = Vec::with_capacity(BASIC_STACK_SIZE + EXTRA_STACK);
        stack.push(TValue::Nil(NilKind::Strict));
        stack
    }
}

impl<'a> LuaState<'a> {
    /// 从 Proto 构建主线程执行态（原 VmState::new；VM 单测用）
    ///
    /// 函数帧布局: stack[base-1] = 函数入口, stack[base+0..base+N] = 寄存器/参数
    /// 当 base=0 时，stack[0] 兼作函数入口和寄存器 0（主函数场景）
    #[cfg(test)]
    pub fn from_proto(
        proto: &Proto<'a>,
        base: usize,
        mut stack: Vec<TValue<'a>>,
        global_state: Rc<RefCell<GlobalShared<'a>>>,
        string_table: Rc<StringTable>,
    ) -> LuaState<'a> {
        // 分配器宿主取自共享块本身：本函数把 globals/registry 注册进 `gs.gc`，
        // 它们必须由**该块自己的**宿主释放（槽内联在引用计数块里，释放发生在
        // 宿主的内层 Box 上）。若此处临时新建宿主并随 `GlobalState` 一起析构，
        // 共享块句柄一旦比执行态活得久，GC 存储里的弱引用就会按已析构宿主释放
        // = use-after-free。共享宿主后，块与执行态各自持一份，先析构谁都不会悬垂。
        let (alloc, alloc_slot) = {
            let shared = global_state.borrow();
            (shared.alloc.clone(), shared.alloc_slot())
        };
        let mut gs = global_state.borrow_mut();
        if base > 0 {
            while stack.len() < base {
                stack.push(TValue::Nil(NilKind::Strict));
            }
        }
        let needed = base + proto.max_stack_size as usize;
        while stack.len() < needed {
            stack.push(TValue::Nil(NilKind::Strict));
        }
        let top = stack.len();

        let globals = {
            let t = Table::new_in(alloc_slot);
            gs.gc.register_table(&t);
            t
        };

        let registry = {
            let t = Table::new_in(alloc_slot);
            gs.gc.register_table(&t);
            t
        };

        let tmnames = Rc::new(init_tmnames(&string_table));
        let owner = Box::new(GlobalState {
            main: std::ptr::null_mut(),
            trap: false,
            tick: std::cell::Cell::new(0),
            twups_linked: false,
            is_in_twups: false,
            gc: NonNull::new(&mut gs.gc).unwrap(),
            globals,
            registry,
            string_table,
            tmnames,
            dmt: DefaultMetatables::new(),
            io_output: Rc::new(RefCell::new(None)),
            file_handles: Rc::new(RefCell::new(std::collections::HashMap::with_hasher(
                crate::objects::FxBuildHasher::default(),
            ))),
            popen_handles: Rc::new(RefCell::new(std::collections::HashSet::with_hasher(
                crate::objects::FxBuildHasher::default(),
            ))),
            io_input_handle: Rc::new(std::cell::Cell::new(None)),
            io_output_handle: Rc::new(std::cell::Cell::new(None)),
            global_state: global_state.clone(),
            ci: None,
            last_c_function: None,
            math_random_state: Rc::new(RefCell::new(None)),
            warn_on: Rc::new(std::cell::Cell::new(true)),
            warn_pending: Rc::new(std::cell::Cell::new(false)),
            main_thread: MyRc::new_in(
                alloc_slot,
                LuaThread {
                    function: None,
                    is_main: true,
                    context: Rc::new(RefCell::new(ThreadContext::default())),
                    c_state: std::cell::Cell::new(std::ptr::null_mut()),
                },
            ),
            weak_tables: Rc::new(RefCell::new(Vec::new())),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(32768),
            finobj_list: Rc::new(RefCell::new(Vec::new())),
            ud_finobj_list: Rc::new(RefCell::new(Vec::new())),
            gc_closing: false,
            exit_requested: None,
            cached_mode_key: std::cell::RefCell::new(None),
            cached_gc_key: std::cell::RefCell::new(None),
            last_gc_estimate: 0,
            mem: MemState::new_complete(alloc),
            io: crate::mock::io_mock::lua_io(),
        });
        let mut s = LuaState::empty_thread();
        s.constants = proto.constants.clone();
        s.code = proto.code.clone();
        s.protos = proto.protos.clone();
        s.stack = stack;
        s.top = top;
        s.base = base;
        s.pc = 0;
        s.num_params = proto.num_params;
        s.is_vararg = proto.is_vararg();
        s.proto_flag = proto.flag;
        s.owner = Some(owner);
        s.bind_exec();
        s
    }
}

impl<'a> GlobalState<'a> {
    /// 关闭期断环 —— 断开仅靠 Rc 自引用环存活的对象的外部引用。
    ///
    /// globals 里存了 `_G -> Table(globals_clone)`，globals_clone 的 Rc 指回
    /// globals 自身的 TableData；Rc 计数永远 ≥ 2，`TableData::drop` 从不被调用，
    /// 内部所有 TValue（含 intern 字符串）都不释放。同理 package <-> package.loaded
    /// （open_selected_libs 把标准库表放进 loaded，loaded["package"] 指回 package）
    /// 与 registry 里的 `_LOADED` / `_PACKAGE` / `_G` 等回指。
    ///
    /// 清空这些表的槽位后，环内对象的外部长引用被释放；**紧随其后必须再跑一次
    /// 完整 GC**（`LuaState::close_state` 中调用顺序：先断环、后 collect_gc），
    /// 由 mark/sweep 把剩下的不可达对象（表/metatable/闭包/proto）清空并释放，
    /// 否则「表↔闭包」这类纯 Rc 环（如 `M.rrotate` 上值捕获 M、metatable ↔ __gc 闭包）
    /// 会在 globals/registry 仍作为 GC 根时被判为可达而逃过清扫，之后 drop 阶段
    /// 释放外部引用时已无第二次回收，环永久存活（ASan 报 indirect leak）。
    /// 对应 C 的 `lua_close` → `luaC_freeallobjects`「关闭即释放全部」语义。
    pub fn break_close_refs(&self) {
        // 先打破 package <-> package.loaded：open_selected_libs 会把各标准库表
        // 放进 package.loaded，且 loaded["package"] 指回 package 自身，构成 Rc 循环。
        // 若不先清空 loaded，则 package 与所有标准库表（含其内部的 intern 字符串）
        // 连同所有 require 过的模块都不会被释放。必须在清空 globals 之前查找。
        //
        // 查 package 表必须与 `base_lib::get_package_table` 一致走 registry["_PACKAGE"]：
        // Lua 脚本可以删除/覆盖全局 `package`（all.lua 结尾会把所有全局变量擦成 undef），
        // 只查 globals 会漏掉这条循环（实测泄漏 188 块 / 26.5KB）。
        let pkg_key = self.intern_str("_PACKAGE");
        let pkg = match self.registry.get(&pkg_key) {
            Some(TValue::Table(t)) => Some(t),
            _ => {
                let g_key = self.intern_str("package");
                match self.globals.get(&g_key) {
                    Some(TValue::Table(t)) => Some(t),
                    _ => None,
                }
            }
        };
        if let Some(pkg) = pkg {
            let loaded_key = self.intern_str("loaded");
            if let Some(TValue::Table(loaded)) = pkg.get(&loaded_key) {
                loaded.clear_for_drop();
            }
        }
        self.globals.clear_for_drop();
        self.registry.clear_for_drop();

        // 其他可能持有 Table 的字段也逐一处理，视实际需要补。
        self.finobj_list.borrow_mut().clear();
        // weak_tables / ud_finobj_list / main_thread 里的 Table 由后续 collect_gc 清扫。
        // 外部字符串（lua_pushexternalstring）的调用方缓冲区不在此释放：其所有权
        // 挂在字符串对象上，字符串不可达（引用计数归零）时由
        // `strings::LongString` 的 drop_payload 调用 falloc 释放 —— 对应 C 的
        // `luaC_freeobj` 对 LSTRMEM 的处理（此处清空 globals/registry/栈即可
        // 让这些字符串随之析构）。
    }
}

impl<'a> Drop for GlobalState<'a> {
    fn drop(&mut self) {
        // 兜底：close_state 已断过环（幂等），此处覆盖未走 close_state 的析构路径。
        self.break_close_refs();
    }
}

// ============================================================================
// 字符串工具
// ============================================================================

pub fn str_to_ls<'a>(alloc: SlotAlloc, table: &StringTable, s: &str) -> TValue<'a> {
    crate::strings::new_lstr(alloc, table, s)
}

fn format_float(f: f64) -> String {
    crate::float_utils::f64_to_string(f)
}

// ============================================================================
// GlobalState — 高层 API
// ============================================================================

impl<'a> GlobalState<'a> {
    pub fn typename(&self, tp: LuaType) -> &'static str {
        match tp {
            LuaType::Nil => "nil",
            LuaType::Boolean => "boolean",
            LuaType::LightUserData => "lightuserdata",
            LuaType::Number => "number",
            LuaType::String => "string",
            LuaType::Table => "table",
            LuaType::Function => "function",
            LuaType::UserData => "userdata",
            LuaType::Thread => "thread",
        }
    }

    // ====== Globals ======

    /// 注册 Rust 原生内置函数到全局表（Rust 风格 API）
    ///
    /// 对应 C API 的 `lua_register(L, name, f)`（内部为 `lua_pushcfunction` + `lua_setglobal`），
    /// 但使用 Rust 原生函数签名 `BuiltinFnPtr`，无需 `unsafe` 或 C ABI 转换。
    ///
    /// ## 与 LightUserData tag 派发的区别
    ///
    /// - 类型安全：函数指针直接调用，无 tag 误判风险
    /// - 性能更优：~5-10 cycles（与 C Lua 持平），比 tag 派发快 2-3 倍
    /// - traceback 友好：`name` 直接存于 TValue，debug.traceback 可直接读取
    ///
    /// ## 参数
    ///
    /// - `name`: 函数名（必须为 `&'static str`，通常用字面量；用于全局表 key 和 traceback）
    /// - `func`: 函数指针，签名为 `fn(&mut GlobalState, a, nargs, nresults) -> Result<(), VmError>`
    ///
    /// ## 示例
    ///
    /// ```ignore
    /// fn my_add<'a>(state: &mut LuaState<'a>, a: usize, nargs: usize, nresults: i32) -> Result<(), VmError<'a>> {
    ///     let x = state.exec.stack[a + 1].as_integer().unwrap_or(0);
    ///     let y = state.exec.stack[a + 2].as_integer().unwrap_or(0);
    ///     state.exec.stack.truncate(a);
    ///     state.exec.stack.push(TValue::Integer(x + y));
    ///     Ok(())
    /// }
    ///
    /// state.set_builtin("my_add", my_add);
    /// state.do_string("print(my_add(3, 4))");  // 输出 7
    /// ```
    pub fn set_builtin(&self, name: &'static std::ffi::CStr, func: BuiltinFnPtr) {
        let name_str = name.to_str().unwrap_or("");
        let key = self.intern_str(name_str);
        let name_ptr = name.as_ptr() as *const u8;
        self.globals
            .set(key, TValue::BuiltinFn(BuiltinFn::impure(func, name_ptr)));
    }

    /// 注册 Rust 原生内置函数到指定表（Rust 风格 API）
    ///
    /// 类似 `set_builtin`，但注册到指定表（如 `math` 表）而非全局表。
    /// 用于库的子函数注册，例如：
    ///
    /// ```ignore
    /// let math_table = state.globals.get(&make_str("math"));
    /// if let TValue::Table(t) = math_table {
    ///     state.set_builtin_in_table(&t, c"abs", math_abs);
    /// }
    /// ```
    pub fn set_builtin_in_table(
        &self,
        table: &Table<'a>,
        name: &'static std::ffi::CStr,
        func: BuiltinFnPtr,
    ) {
        let name_str = name.to_str().unwrap_or("");
        let key = self.intern_str(name_str);
        let name_ptr = name.as_ptr() as *const u8;
        table.set(key, TValue::BuiltinFn(BuiltinFn::impure(func, name_ptr)));
    }

    // ====== GC 控制 ======

    pub fn gc_stop(&mut self) {
        unsafe { self.gc.as_mut().gc_stop.set(1) };
    }

    pub fn gc_restart(&mut self) {
        unsafe { self.gc.as_mut().gc_stop.set(0) };
    }

    pub fn gc_gen(&mut self) {
        unsafe { self.gc.as_mut().set_mode(crate::gc::GCMode::Generational) };
    }

    pub fn gc_inc(&mut self) {
        unsafe { self.gc.as_mut().set_mode(crate::gc::GCMode::Incremental) };
    }

    // ====== Diagnostics ======

    pub fn warning(&mut self, msg: &str, tocont: bool) {
        use std::io::Write;
        if !self.warn_pending.get() {
            // warnfon / warnfoff 行为：检查控制消息
            if !tocont && msg.starts_with('@') {
                let ctl = &msg[1..];
                match ctl {
                    "off" => {
                        self.warn_on.set(false);
                        self.warn_pending.set(false);
                    }
                    "on" => {
                        self.warn_on.set(true);
                        self.warn_pending.set(false);
                    }
                    _ => {} // 未知控制消息，忽略
                }
                return;
            }
            if !self.warn_on.get() {
                return;
            }
            // warnfon: 输出前缀 + 消息
            let stderr = std::io::stderr();
            let mut h = stderr.lock();
            let _ = h.write_all(b"Lua warning: ");
            let _ = h.write_all(msg.as_bytes());
            if tocont {
                self.warn_pending.set(true);
            } else {
                let _ = h.write_all(b"\n");
                self.warn_pending.set(false);
            }
            let _ = h.flush();
        } else {
            // warnfcont 行为：不检查控制消息，直接输出
            let stderr = std::io::stderr();
            let mut h = stderr.lock();
            let _ = h.write_all(msg.as_bytes());
            if tocont {
                self.warn_pending.set(true);
            } else {
                let _ = h.write_all(b"\n");
                self.warn_pending.set(false);
            }
            let _ = h.flush();
        }
    }

    pub fn check_version(&self) {}

    // ====== Call Meta ======

    pub fn call_meta(&self, _idx: isize, _event: &str) -> bool {
        false
    }

    pub fn error(&mut self, msg: &str) -> String {
        msg.to_string()
    }

    // ====== Push C Function ======

    // ====== Load Code ======

    // ====== pcall ======

    // ====== Open Libs ======

    // ====== Hook ======

    pub fn set_hook(&mut self, _hook: Option<(usize, usize)>, _mask: i32, _count: i32) {}

    // ====== String Helpers ======

    /// 分配器槽：取自 [`AllocOwner`] 中的稳定地址（分配器宿主的内层 Box 位于
    /// 固定堆块 → 槽自 `GlobalState` 构造期起即有效，且 `GlobalState` 移动/装箱
    /// 都不影响；`lua_setallocf` 原地替换内层内容后自动跟随）。
    /// 用 `&self` 取槽以便与 `&self.string_table` 共存（见 [`crate::strings::AllocSlot`]）。
    #[inline]
    pub fn alloc_slot(&self) -> SlotAlloc {
        SlotAlloc::from_owner(&self.mem.allocator)
    }

    pub fn intern_str(&self, s: &str) -> TValue<'a> {
        str_to_ls(self.alloc_slot(), &self.string_table, s)
    }

    pub fn intern(&self, s: &str) -> TValue<'a> {
        str_to_ls(self.alloc_slot(), &self.string_table, s)
    }

    /// 从 owned String 创建 LuaString，长字符串路径直接 consume 避免 clone。
    /// perf: str_format 等产生临时长 String 的场景，消除 with_nul 的 to_string() clone。
    pub fn intern_str_owned(&self, s: String) -> TValue<'a> {
        crate::strings::new_lstr_from_string(self.alloc_slot(), &self.string_table, s)
    }
}

// ============================================================================
// load_file 辅助函数
// ============================================================================

/// 跳过 UTF-8 BOM（EF BB BF）。若 BOM 不完整则保留原字节，与 C 的 `skipBOM` 行为一致。
macro_rules! skip_bom_fn {
    // 入口：$mut 可以是空或 `mut`
    ($name:ident, $($mut:tt)?) => {
        pub fn $name(bytes: & $($mut)? [u8]) -> & $($mut)? [u8] {
            const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
            if bytes.starts_with(BOM) {
                let n = BOM.len();
                // 用内部辅助宏根据 $mut 展开不同的切片方式
                skip_bom_fn!(@subslice bytes, $($mut)?, n)
            } else {
                bytes
            }
        }
    };
    // 不可变：普通索引
    (@subslice $bytes:ident, , $n:ident) => {
        & $bytes[$n..]
    };
    // 可变：split_at_mut
    (@subslice $bytes:ident, mut, $n:ident) => {
        $bytes.split_at_mut($n).1
    };
}

// 生成两个函数，无需重复写 starts_with 和 if 逻辑
skip_bom_fn!(skip_bom,);
skip_bom_fn!(skip_bom_mut, mut);

/// 跳过可选的首行注释（以 '#' 开头的 shebang/Unix exec 行）。
///
/// 返回三元组：`(是否跳过了首行, 首字符, 首字符之后的剩余字节)`。
/// 与 C 的 `skipcomment` 一致：`first` 是从流中读取出来的字符，
/// `rest` 包含 `first`，`load_bytes` 不需要再把 `first` 放回缓冲区。
fn skip_comment(bytes: &[u8]) -> (bool, Option<u8>, &[u8], Option<usize>) {
    if bytes.first() == Some(&b'#') {
        let mut pos = 1;
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        // 同时消费换行符本身，与 C 的 `skipcomment` 一致。
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
        // first 是注释后的第一个字符；rest 是该字符之后的字节
        let first = bytes.get(pos).copied();
        let rest_start = (pos).min(bytes.len());
        (true, first, &bytes[rest_start..], Some(rest_start))
    } else {
        // first 是第一个字符；rest 是该字符之后的字节
        let first = bytes.first().copied();
        let rest = if bytes.is_empty() { &[] } else { &bytes[0..] };
        (false, first, rest, None)
    }
}

/// 判断 `mode` 是否允许文本块。
fn mode_allows_text(mode: Option<&str>) -> bool {
    match mode {
        None => true,
        Some(m) => m.contains('t') || (!m.contains('b') && !m.contains('t')),
    }
}

/// 判断 `mode` 是否允许二进制块。
fn mode_allows_binary(mode: Option<&str>) -> bool {
    match mode {
        None => true,
        Some(m) => m.contains('b'),
    }
}

/// 将源码字节解码为 Rust `String`。
///
/// Lua 源码本质上是字节流。若字节序列是合法 UTF-8，则直接解码；
/// 否则按 ISO-8859-1 逐字节映射为对应 Unicode 码点。这样既能正确处理
/// 常见的 UTF-8 文件，也能处理 `tests_lua/strings.lua` 等 ISO-8859 文件。
fn decode_source_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一线程内的多个 `LuaState` 必须各自持有**不同**的分配器。
    ///
    /// 禁止「每线程一个分配器」的假定：C API 下同线程可有多个 state，
    /// 各自 `lua_newstate(allocf, ud)` / `lua_setallocf` 挂不同的分配器。
    #[test]
    fn states_have_independent_allocators() {
        let mut s1 = LuaState::default();
        let s2 = LuaState::default();
        assert_ne!(
            s1.alloc_slot().slot(),
            s2.alloc_slot().slot(),
            "同线程的两个 state 不得共用分配器槽"
        );

        // 替换 s1 的 allocf 不得影响 s2（槽地址各自独立）
        let before1 = s1.alloc_slot().slot();
        let before2 = s2.alloc_slot().slot();
        s1.g_mut().mem.allocator.set(Box::new(DefaultAllocator));
        assert_eq!(
            before1,
            s1.alloc_slot().slot(),
            "set 应原地替换，槽地址不变"
        );
        assert_eq!(before2, s2.alloc_slot().slot(), "不得影响另一 state");
    }

    #[test]
    fn test_stack_init_matches_cpp() {
        // 验证 stack_init: 参考 lstate.cpp L158-169
        let state = LuaState::default();
        // L->top = stack + 1 → gettop() 必须返回 1
        assert_eq!(
            state.gettop(),
            1,
            "stack length must be 1 (function entry slot)"
        );
        // stack[0] 必须是函数入口 nil
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));
        // 容量 = BASIC_STACK_SIZE + EXTRA_STACK
        assert_eq!(state.stack.capacity(), BASIC_STACK_SIZE + EXTRA_STACK);
    }

    /// 共享块的 GC 存储比执行态活得久（本用例自己持有 `gs` 句柄，且三个执行态
    /// 各自析构）——因此本用例必须在 Miri（Tree Borrows）下干净：块内对象的释放
    /// 只能走**块自己的**分配器宿主。若宿主随某个执行态提前析构，末次弱引用释放
    /// 会按悬垂槽释放块（`constructing invalid value of type &Box<dyn Allocator>`）。
    #[test]
    fn test_stack_init_from_proto() {
        let string_table = Rc::new(StringTable::new());
        // 验证 from_proto 的栈初始化: base > 0 时必须保证函数入口槽
        let proto = Proto {
            num_params: 0,
            flag: 0,
            max_stack_size: 10,
            size_upvalues: 0,
            size_k: 0,
            size_code: 0,
            size_line_info: 0,
            size_p: 0,
            size_loc_vars: 0,
            size_abs_line_info: 0,
            line_defined: 0,
            last_line_defined: 0,
            constants: Rc::new(vec![]),
            code: Rc::new(vec![]),
            protos: Rc::new(vec![]),
            upvalues: Rc::new(vec![]),
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let gs = Rc::new(RefCell::new(GlobalShared::new(
            AllocOwner::new(Box::new(DefaultAllocator)),
            &string_table,
        )));

        // case 1: base=0, empty stack → main function scenario
        let state = LuaState::from_proto(&proto, 0, vec![], Rc::clone(&gs), string_table.clone());
        assert_eq!(state.base, 0);
        assert_eq!(
            state.stack.len(),
            10,
            "base=0 with max_stack_size=10 must allocate 10 register slots"
        );

        // case 2: base=1, empty stack → called function scenario
        let state = LuaState::from_proto(&proto, 1, vec![], Rc::clone(&gs), string_table.clone());
        assert_eq!(state.base, 1);
        assert_eq!(
            state.stack.len(),
            11,
            "base=1 with max_stack_size=10 must allocate 1+10=11 slots"
        );
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));

        // case 3: base=1, stack with args → called function
        let state = LuaState::from_proto(
            &proto,
            1,
            vec![TValue::Nil(NilKind::Strict), TValue::Integer(42)],
            gs.clone(),
            string_table.clone(),
        );
        assert_eq!(state.base, 1);
        assert_eq!(
            state.stack.len(),
            11,
            "base=1 with max_stack_size=10 must allocate 1+10=11 slots"
        );
        assert!(matches!(state.stack[0], TValue::Nil(NilKind::Strict)));
        assert_eq!(state.stack[1], TValue::Integer(42));
    }
    #[test]
    fn test_stack_init_default() {
        let state = LuaState::default();
        assert_eq!(state.gettop(), 1, "Default must init stack via new()");
    }

    // ------------------------------------------------------------------------
    // load_file 编码与文件加载测试
    // ------------------------------------------------------------------------

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "lua_rs_load_file_test_{}_{}",
            name,
            std::process::id()
        ));
        p
    }

    fn write_tmp(name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = tmp_path(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_skip_bom() {
        assert_eq!(skip_bom(b"\xef\xbb\xbfhello"), b"hello");
        assert_eq!(skip_bom(b"\xef\xbbhello"), b"\xef\xbbhello");
        assert_eq!(skip_bom(b"hello"), b"hello");
    }

    #[test]
    fn test_skip_comment() {
        let (skipped, first, rest, _) = skip_comment(b"#!/bin/lua\nprint(1)");
        assert!(skipped);
        assert_eq!(first, Some(b'p'));
        assert_eq!(rest, b"print(1)");

        let (skipped, first, rest, _) = skip_comment(b"-- no shebang\nreturn");
        assert!(!skipped);
        assert_eq!(first, Some(b'-'));
        assert_eq!(rest, b"-- no shebang\nreturn");

        let (skipped, first, rest, _) = skip_comment(b"#only shebang");
        assert!(skipped);
        assert_eq!(first, None);
        assert!(rest.is_empty());
    }

    #[test]
    fn test_decode_source_bytes_utf8() {
        let bytes = "local x = 1 -- 中文".as_bytes();
        assert_eq!(decode_source_bytes(bytes), "local x = 1 -- 中文");
    }

    #[test]
    fn test_decode_source_bytes_iso8859() {
        // ISO-8859-1 字节：á é í
        // 在 Rust String 中每个字节被映射为对应 Unicode 码点，UTF-8 编码后长度会变化，
        // 因此这里验证字符数量与码点值保持一致。
        let bytes: Vec<u8> = vec![0xe1, 0xe9, 0xed];
        let s = decode_source_bytes(&bytes);
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[0] as u32, 0xe1);
        assert_eq!(chars[1] as u32, 0xe9);
        assert_eq!(chars[2] as u32, 0xed);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_load_file_decodes_iso8859_strings() {
        let mut state = LuaState::default();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests_lua/strings.lua");
        let status = state.load_file(Some(path));
        assert_eq!(
            status,
            0,
            "load_file should succeed: {:?}",
            state.to_string(-1)
        );
        assert!(
            matches!(state.stack.last(), Some(TValue::LClosure(_))),
            "stack top should be a closure"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_load_file_skips_shebang_all() {
        let mut state = LuaState::default();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests_lua/all.lua");
        let status = state.load_file(Some(path));
        assert_eq!(
            status,
            0,
            "load_file should succeed: {:?}",
            state.to_string(-1)
        );
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_load_file_missing() {
        let mut state = LuaState::default();
        let status = state.load_file(Some("/nonexistent/path/file.lua"));
        assert_eq!(status, ERR_FILE);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("cannot open"), "unexpected error: {}", msg);
    }

    #[test]
    fn test_load_file_binary_signature() {
        let mut content = b"\x1bLua\x55".to_vec();
        content.extend_from_slice(&[0; 10]);
        let path = write_tmp("binary", &content);
        let mut state = LuaState::default();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("binary chunk"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_mode_text_rejects_binary() {
        let mut content = b"\x1bLua\x55".to_vec();
        content.extend_from_slice(&[0; 10]);
        let path = write_tmp("bin_text_mode", &content);
        let mut state = LuaState::default();
        let status = state.load_filex(Some(path.to_str().unwrap()), Some("t"));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("mode is 'text'"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_mode_binary_rejects_text() {
        let path = write_tmp("text_bin_mode", b"return 42\n");
        let mut state = LuaState::default();
        let status = state.load_filex(Some(path.to_str().unwrap()), Some("b"));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(
            msg.contains("mode is 'binary'"),
            "unexpected error: {}",
            msg
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_bom() {
        let path = write_tmp("bom", b"\xef\xbb\xbfreturn 42\n");
        let mut state = LuaState::default();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(
            status,
            0,
            "load_file should succeed: {:?}",
            state.to_string(-1)
        );
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_shebang_only() {
        let path = write_tmp("shebang_only", b"#!/usr/bin/env lua\n");
        let mut state = LuaState::default();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(
            status,
            0,
            "empty shebang file should load: {:?}",
            state.to_string(-1)
        );
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_latin1_in_string_literal() {
        // ISO-8859-1 字节直接出现在字符串字面量中
        let mut bytes: Vec<u8> = b"local s = \"".to_vec();
        bytes.extend_from_slice(&[0xe1, 0xe9, 0xed]);
        bytes.extend_from_slice(b"\"\nreturn #s\n");
        let path = write_tmp("latin1_str", &bytes);
        let mut state = LuaState::default();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(
            status,
            0,
            "latin1 string literal should load: {:?}",
            state.to_string(-1)
        );
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_shebang_with_latin1() {
        // 首行是 shebang，后续包含 ISO-8859-1 字节
        let mut bytes: Vec<u8> = b"#!/bin/lua\nlocal s = \"".to_vec();
        bytes.extend_from_slice(&[0xc1, 0xc9, 0xcd]);
        bytes.extend_from_slice(b"\"\n");
        let path = write_tmp("shebang_latin1", &bytes);
        let mut state = LuaState::default();
        let status = state.load_file(Some(path.to_str().unwrap()));
        assert_eq!(
            status,
            0,
            "shebang + latin1 should load: {:?}",
            state.to_string(-1)
        );
        assert!(matches!(state.stack.last(), Some(TValue::LClosure(_))));
        let _ = std::fs::remove_file(&path);
    }
}
