use crate::debug::runerror;
use crate::execute::{VmError, VmExecutor, VmResult};
use crate::gc::{GCObjectHeader, GCState};
use crate::objects::{
    CallFrame, Instruction, LClosure, LuaThread, LuaType, NilKind, Proto, TValue, TableData,
    ThreadContext, ThreadStatus, UpVal, UpValRef, UpvalDesc,
};
use crate::strings::{LuaString, StringTable};
use crate::table::Table;
use crate::tm::DefaultMetatables;
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::rc::Rc;

const EOFMARK: &str = "<eof>";

pub const LUA_YIELD: i32 = 1;
pub const ERR_RUN: i32 = 2;
pub const ERR_SYNTAX: i32 = 3;
pub const ERR_FILE: i32 = 6; // LUA_ERRERR + 1, used by luaL_loadfilex
pub const MULT_RET: i32 = -1;

const LUA_SIGNATURE: &[u8] = b"\x1bLua";
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

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
pub const MAXSTACK_BYSIZET: usize =
    (usize::max_value() / std::mem::size_of::<TValue>()) - STACKERRSPACE;
pub const MAXSTACK: usize = if LUAI_MAXSTACK < MAXSTACK_BYSIZET {
    LUAI_MAXSTACK
} else {
    MAXSTACK_BYSIZET
};
pub const ERRORSTACKSIZE: usize = MAXSTACK + STACKERRSPACE;

/// pcall 类型 — 区分 pcall 和 xpcall 的 error 处理行为
#[derive(Clone, Debug)]
pub enum PcallKind {
    /// pcall: error 时返回 (false, error)
    Pcall,
    /// xpcall: error 时调用 handler(error)，返回 (false, handler_result)
    Xpcall { handler: TValue },
}

/// pcall 保护状态 — 对应 C Lua 的 CIST_YPCALL CallInfo
///
/// call_pcall/call_xpcall 在调用 state.pcall 前 push 此结构。
/// yield 穿过 pcall 后，pcall 的 C 函数栈帧被销毁，但保护状态保留在栈中。
/// 当 inner_func 后续执行 error 或正常返回时，由 execute_loop 检查并处理
/// （对应 C Lua 的 precover + finishpcallk 机制）。
#[derive(Clone, Debug)]
pub struct PcallProtection {
    /// pcall 调用者的执行上下文（saved_*）— 用于恢复 pcall 调用者的执行
    pub saved_code: Vec<Instruction>,
    pub saved_constants: Vec<TValue>,
    pub saved_upval_descs: Vec<UpvalDesc>,
    pub saved_protos: Vec<Rc<Proto>>,
    pub saved_base: usize,
    pub saved_pc: usize,
    pub saved_num_params: u8,
    pub saved_is_vararg: bool,
    pub saved_proto_flag: u8,
    pub saved_nextraargs: i32,
    pub saved_closure_upvals: Vec<UpValRef>,
    pub saved_tbc_list: Option<usize>,
    /// pcall 的 func 位置（栈索引）— 用于截断栈和放置返回值
    pub func_idx: usize,
    /// pcall 期望的返回值数量（-1 = MULTRET）
    pub nresults: i32,
    /// pcall 类型 — 区分 pcall 和 xpcall 的 error/return 处理
    pub pcall_kind: PcallKind,
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
}

pub struct GlobalState {
    pub gcstopem: bool,
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
// LuaState — 合并 VmState + LuaState 的所有字段
// ============================================================================

pub struct LuaState {
    // 执行上下文（原 VmState）
    pub constants: Vec<TValue>,
    pub code: Vec<Instruction>,
    pub upval_descs: Vec<UpvalDesc>,
    pub protos: Vec<Rc<Proto>>,
    pub top: usize,
    pub base: usize,
    pub pc: usize,
    pub trap: bool,
    pub num_params: u8,
    pub is_vararg: bool,
    /// 当前执行函数原型的 flag（PF_VAHID / PF_VATAB / PF_FIXED）
    pub proto_flag: u8,
    /// PF_VAHID 模式下隐藏变参的数量（对应 C 的 ci->u.l.nextraargs）
    pub nextraargs: i32,
    pub closure_upvals: Vec<UpValRef>,
    /// 全局 open upvalue 存储 — 不随函数调用/返回保存/恢复（对应 C 的 L->openupval 链表节点存储）
    /// open_upval 链表索引此 vec，tbc_list 也索引此 vec
    pub open_upvals: Vec<UpValRef>,
    pub open_upval: Option<usize>,
    pub tbc_list: Option<usize>,
    pub twups_linked: bool,
    pub is_in_twups: bool,

    // 公用字段
    pub stack: Vec<TValue>,
    pub gc: Rc<GCState>,

    // 高层 API 字段（原 LuaState）
    pub globals: Table,
    pub registry: Table,
    pub string_table: StringTable,

    // C API 导出层使用：当前 C 函数帧的 func 位置（0-based 栈索引）。
    // C API 的正索引相对于此位置；Lua 代码路径不使用此字段。
    // 0 表示栈底（主线程初始状态）。
    pub api_func_base: usize,
    // C 函数调用嵌套计数（对应 C 的 L->nCcalls），用于检测 C 栈溢出
    pub n_ccalls: u32,
    // 非可 yield 调用计数（对应 C 的 nCcalls 高 16 位 / incnny）
    // 常规 C 函数调用递增此计数，pcall/xpcall 例外（CIST_YPCALL）
    // coroutine.isyieldable() 检查此计数是否为 0
    pub n_ny_calls: u32,
    pub dmt: DefaultMetatables,
    pub stdout: Box<dyn Write>,
    /// io.output 设置的当前输出流 — None 表示使用 stdout
    /// 对应 C liolib 中存储在 registry[IO_OUTPUT] 的默认输出文件句柄
    pub io_output: Option<Box<dyn Write>>,
    /// 文件句柄注册表 — key 是 UserData 的 gc_header.ptr_id，value 是 FILE* 指针
    /// 对应 C 的 luaL_Stream 中存储的 FILE*。UserData 本身不存数据，通过此 map 关联。
    pub file_handles: std::collections::HashMap<u32, *mut libc::FILE>,
    /// 标记哪些文件句柄是 io.popen 创建的（关闭时用 pclose 而非 fclose）
    /// 对应 C 的 LStream.closef = &io_pclose
    pub popen_handles: std::collections::HashSet<u32>,
    /// 当前默认输入流的 UserData ptr_id — None 表示使用 io.stdin
    /// 对应 C 的 registry[IO_INPUT]
    pub io_input_handle: Option<u32>,
    /// 当前默认输出流的 UserData ptr_id — None 表示使用 io.stdout
    /// 对应 C 的 registry[IO_OUTPUT]
    pub io_output_handle: Option<u32>,
    pub global_state: Rc<GlobalState>,
    pub ci: Option<Box<CallInfo>>,
    /// 调用栈信息，用于构建堆栈回溯 — 对应 C 的 CallInfo 链表
    /// 每个元素是 (source, line, function_name)
    pub call_info: Vec<CallInfoEntry>,
    /// 最后一次错误的堆栈回溯字符串
    pub last_traceback: String,
    /// 最后一次错误的格式化消息（含 source:line 前缀）
    pub last_error_msg: String,
    /// 最后一次错误的原始值（保留原始 TValue 类型，如 error(100) 的数字 100）
    /// coroutine.close 需要返回此原始值
    pub last_error_value: Option<TValue>,
    /// C API 的 lua_error 抛错暂存槽 — 对应 C 的 longjmp 错误值
    /// lua_error 设置此字段并 panic，pcall_c_function 用 catch_unwind 捕获后取出
    pub pending_error: Option<TValue>,
    /// 标记错误消息是否应跳过 source:line 前缀
    /// error() level=0 或非字符串错误值时为 true，build_traceback 不再添加前缀
    pub error_no_prefix: bool,
    /// pcall 内部发生 yield 时暂存的 yield 值，供调用者传播
    pub pending_yield: Option<Vec<TValue>>,
    /// 当前正在调用的 C 函数名（用于 traceback）— None 表示不在 C 函数中
    pub last_c_function: Option<String>,
    /// 数学库随机数生成器状态 — 对应 C 的 RanState (math.random/randomseed)
    pub math_random_state: Option<Box<crate::stdlib::math_lib::RandState>>,
    /// debug hook 函数 — 对应 C 的 L->hook
    pub hook_func: Option<TValue>,
    /// debug hook 掩码 — 对应 C 的 L->hookmask
    pub hook_mask: i32,
    /// debug hook count — 对应 C 的 L->basehookcount
    pub hook_count: i32,
    /// 当前 hook count (每条指令递减) — 对应 C 的 L->hookcount
    pub current_hook_count: i32,
    /// 上次 line hook 检查的 pc — 对应 C 的 L->oldpc（指令索引，不是行号）
    pub hook_old_pc: i32,
    /// 是否允许调用 hook — 对应 C 的 L->allowhook
    /// 在 hook 执行期间设为 false，防止递归调用
    pub allowhook: bool,
    /// warning 系统是否开启 — 对应 C 的 G(L)->warnf == warnfon
    pub warn_on: bool,
    /// warning 是否有未完成的消息（tocont=true 后等待下一段）— 对应 C 的 warnfcont 状态
    pub warn_pending: bool,
    /// 主线程对象（用于 coroutine.running() 在主线程返回 thread）
    pub main_thread: LuaThread,
    /// 调用栈 — 保存 caller 的 VM 执行上下文（原 execute_loop 局部变量，提升为字段以支持协程挂起）
    pub call_stack: Vec<CallFrame>,
    /// 当前活动协程的上下文 — None 表示主线程执行中
    pub current_thread: Option<Rc<RefCell<ThreadContext>>>,
    /// coroutine.wrap 创建的协程列表 — 通过 tag (710+idx) 索引
    /// None 表示协程已完成或出错
    pub wrap_coros: Vec<Option<LuaThread>>,
    /// call_wrap_call 执行期间，调用者栈（含活跃的 wrap table 引用）暂存于此，
    /// 让 GC 能看到内层协程引用，避免误判为不可达。
    /// 嵌套 wrap 调用时按栈顺序 push/pop。
    pub caller_gc_stacks: Vec<Vec<TValue>>,
    /// pcall 保护栈 — 对应 C Lua 的 CIST_YPCALL CallInfo 链
    /// 当 pcall 保护可 yield 的 Lua 函数时，push 保护状态。
    /// yield 穿过 pcall 后，保护状态保留。
    /// execute_loop 收到 error/return 时检查此栈，处理 pcall 的返回/error。
    pub pcall_protection_stack: Vec<PcallProtection>,
    /// 弱引用表列表 — setmetatable 设置 __mode 时注册，collectgarbage 时清理
    /// 使用 Weak 引用避免阻止表本身的回收
    pub weak_tables: Vec<std::rc::Weak<std::cell::RefCell<TableData>>>,
    /// op_concat 的 GC 计数器 — 字符串不注册到 GC metas，用计数器限制
    /// 每 concat_gc_interval 次 op_concat 触发一次 GC（清理弱引用表等）
    pub concat_gc_counter: std::cell::Cell<usize>,
    pub concat_gc_interval: std::cell::Cell<usize>,
    /// 有 __gc 元方法的对象列表 — setmetatable 设置 __gc 时注册
    /// GC 时检查不可达的对象，调用其 finalizer 后再释放
    pub finobj_list: Vec<Table>,
    /// 有 __gc 元方法的 UserData 列表 — FILE* 等通过默认元表设置的 userdata
    /// GC 时检查不可达的 UserData，调用其 finalizer（fclose）后释放
    pub ud_finobj_list: Vec<crate::objects::Udata>,
    /// 状态正在关闭 — 对应 C 的 g->gcstp & GCSTPCLS
    /// true 时不再注册新的 finalizer 对象（close 中创建的对象不会被 finalize）
    pub gc_closing: bool,
    /// os.exit 请求的退出码 — 在 finalizer 中调用 os.exit(code, true) 时设置，
    /// close_state 处理完所有 finalizer 后据此退出进程。
    pub exit_requested: Option<i32>,
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
    pub last_error_call_info: Option<Vec<CallInfoEntry>>,
    /// 最后一个出错的 __close 的 CallInfoEntry — 对应 C Lua 中 longjmp 跳过 callclosemethod
    /// 的弹出代码，CallInfo 节点保留在链表中。call_close_method 出错时 pop __close 帧并保存
    /// 到此字段。state.pcall 保存 call_info 快照时将此帧追加到快照末尾，让 xpcall 的错误
    /// 处理函数（如 debug.traceback）能看到 __close 帧。
    pub last_close_frame: Option<CallInfoEntry>,
    /// close continuation 的 pending error — 对应 C Lua 的 CIST_RECST 保存的错误状态。
    /// 当 __close 出错时，execute_loop 错误处理分支将 error 保存到此字段，然后调用
    /// func::close 继续关闭剩余 TBC 变量。func::close 处理完毕后检查此字段，
    /// 若有 pending error 则传播给上层 pcall。
    pub close_error_status: Option<TValue>,
    /// coroutine.close() 关闭自身时设置 — 对应 C Lua 的 lua_closethread(co, L) 中
    /// co == L 的场景。C Lua 中 coroutine.close() 会立即调用 luaF_close 关闭所有 TBC 变量，
    /// 并通过 luaD_throwbaselevel 抛到协程 base level。我们的实现未完整支持此语义，
    /// 改为设置此标志，让后续 OP_RETURN 的 func::close 使用不可 yield 模式 (yy=0)，
    /// 使 __close 中的 yield 失败（对应 nny > 0 场景）。
    /// call_resume 开始时清除。
    pub force_noyield_close: bool,
}

/// 调用栈条目 — 用于堆栈回溯和 debug.getinfo
#[derive(Debug, Clone)]
pub struct CallInfoEntry {
    pub source: String,
    pub line: i32,
    pub name: String,
    pub is_c: bool,
    /// Lua 函数引用（C 函数为 None）
    pub closure: Option<Box<crate::objects::LClosure>>,
    /// 栈帧基址（对应 C 的 ci->func + 1）
    pub base: usize,
    /// 调用时的 PC（用于计算 currentline）
    pub saved_pc: usize,
    /// 函数名类型: "local", "global", "method", "field", "hook", ""
    pub namewhat: String,
    /// 调用者的 proto_flag（PF_VAHID / PF_VATAB）— 用于 debug.getlocal level > 1
    pub proto_flag: u8,
    /// 调用者的 nextraargs — 用于 debug.getlocal level > 1 的 vararg 访问
    pub nextraargs: i32,
    /// 是否为尾调用（对应 C 的 CIST_TAIL）— debug.getinfo(1).istailcall
    pub is_tailcall: bool,
}

fn G(l: &LuaState) -> &GlobalState {
    &l.global_state
}

// ============================================================================
// 构造
// ============================================================================

impl LuaState {
    /// 对应 C 的 lua_newstate → stack_init + resetCI
    ///
    /// stack_init: 预分配 BASIC_STACK_SIZE + EXTRA_STACK 个槽位容量
    /// L->stack_last = stack + BASIC_STACK_SIZE
    /// resetCI: ci->func = stack[0], ci->top = stack[0] + 1 + LUA_MINSTACK
    /// L->top = stack + 1  (函数入口 nil 在位索引 0)
    ///
    /// 验证: gettop() 必须返回 1（函数入口槽）
    pub fn new() -> Self {
        let gc = Rc::new(GCState::default_incremental());
        let globals = {
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };
        let registry = {
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };
        registry.set(TValue::Integer(2), TValue::Table(globals.clone()));

        let stack = Self::init_stack();
        let top = stack.len();

        LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            top,
            base: 0,
            pc: 0,
            trap: false,
            num_params: 0,
            is_vararg: false,
            proto_flag: 0,
            nextraargs: 0,
            closure_upvals: Vec::new(),
            open_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack,
            gc,
            globals,
            registry,
            string_table: StringTable::new(),
            api_func_base: 0,
            n_ccalls: 0,
            n_ny_calls: 0,
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            io_output: None,
            file_handles: std::collections::HashMap::new(),
            popen_handles: std::collections::HashSet::new(),
            io_input_handle: None,
            io_output_handle: None,
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_error_value: None,
            pending_error: None,
            error_no_prefix: false,
            pending_yield: None,
            last_c_function: None,
            math_random_state: None,
            hook_func: None,
            hook_mask: 0,
            hook_count: 0,
            current_hook_count: 0,
            hook_old_pc: 0,
            allowhook: true,
            warn_on: true,
            warn_pending: false,
            main_thread: LuaThread {
                stack: Vec::new(),
                status: ThreadStatus::OK,
                function: None,
                is_main: true,
                context: Rc::new(RefCell::new(ThreadContext::default())),
            },
            call_stack: Vec::with_capacity(32),
            current_thread: None,
            wrap_coros: Vec::new(),
            caller_gc_stacks: Vec::new(),
            pcall_protection_stack: Vec::new(),
            weak_tables: Vec::new(),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(4096),
            finobj_list: Vec::new(),
            ud_finobj_list: Vec::new(),
            gc_closing: false,
            exit_requested: None,
            transferinfo_ftransfer: 0,
            transferinfo_ntransfer: 0,
            pending_return_adjust: None,
            last_error_call_info: None,
            last_close_frame: None,
            close_error_status: None,
            force_noyield_close: false,
        }
    }

    /// 初始化栈: 对应 C 的 stack_init
    /// 分配 BASIC_STACK_SIZE + EXTRA_STACK 容量，推入函数入口 nil
    /// stack[0] = nil (函数入口, ci->func)
    /// top = stack + 1 (1 个元素在用)
    fn init_stack() -> Vec<TValue> {
        let mut stack = Vec::with_capacity(BASIC_STACK_SIZE + EXTRA_STACK);
        stack.push(TValue::Nil(NilKind::Strict));
        stack
    }

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
    pub fn growstack(&mut self, n: usize, raiseerror: bool) -> Result<(), VmError> {
        let size = self.stack.len();
        if size > MAXSTACK {
            // 栈已超过最大值，线程正在使用为错误保留的额外空间
            debug_assert_eq!(size, ERRORSTACKSIZE);
            if raiseerror {
                // 对应 C 的 luaD_errerr (栈错误发生在消息处理器内)
                // 简化: 直接返回 StackError
            }
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
    pub fn reallocstack(&mut self, newsize: usize, _raiseerror: bool) -> Result<(), VmError> {
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

    /// 对应 C 的 relstack: 将指针转为偏移量
    /// Rust 版本: 无操作 (Vec 自行管理内存，使用索引)
    fn relstack(&mut self) {
        // no-op in Rust
    }

    /// 对应 C 的 correctstack: 将偏移量转回指针
    /// Rust 版本: 无操作 (Vec 自行管理内存，使用索引)
    fn correctstack(&mut self) {
        // no-op in Rust
    }

    /// 使用已有的 GCState 创建 LuaState
    pub fn with_gc(gc: Rc<GCState>) -> Self {
        let globals = {
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };
        let registry = {
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };
        registry.set(TValue::Integer(2), TValue::Table(globals.clone()));

        let stack = Self::init_stack();
        let top = stack.len();

        let state = LuaState {
            constants: Vec::new(),
            code: Vec::new(),
            upval_descs: Vec::new(),
            protos: Vec::new(),
            top,
            base: 0,
            pc: 0,
            trap: false,
            num_params: 0,
            is_vararg: false,
            proto_flag: 0,
            nextraargs: 0,
            closure_upvals: Vec::new(),
            open_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack,
            gc,
            globals,
            registry,
            string_table: StringTable::new(),
            api_func_base: 0,
            n_ccalls: 0,
            n_ny_calls: 0,
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            io_output: None,
            file_handles: std::collections::HashMap::new(),
            popen_handles: std::collections::HashSet::new(),
            io_input_handle: None,
            io_output_handle: None,
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_error_value: None,
            pending_error: None,
            error_no_prefix: false,
            pending_yield: None,
            last_c_function: None,
            math_random_state: None,
            hook_func: None,
            hook_mask: 0,
            hook_count: 0,
            current_hook_count: 0,
            hook_old_pc: 0,
            allowhook: true,
            warn_on: true,
            warn_pending: false,
            main_thread: LuaThread {
                stack: Vec::new(),
                status: ThreadStatus::OK,
                function: None,
                is_main: true,
                context: Rc::new(RefCell::new(ThreadContext::default())),
            },
            call_stack: Vec::with_capacity(32),
            current_thread: None,
            wrap_coros: Vec::new(),
            caller_gc_stacks: Vec::new(),
            pcall_protection_stack: Vec::new(),
            weak_tables: Vec::new(),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(4096),
            finobj_list: Vec::new(),
            ud_finobj_list: Vec::new(),
            gc_closing: false,
            exit_requested: None,
            transferinfo_ftransfer: 0,
            transferinfo_ntransfer: 0,
            pending_return_adjust: None,
            last_error_call_info: None,
            last_close_frame: None,
            close_error_status: None,
            force_noyield_close: false,
        };
        state
    }

    /// 执行 Lua 字节码 (顶层主函数)
    /// base=0: stack[0] 兼作函数入口和寄存器 0
    pub fn execute(&mut self, proto: &Proto) -> Result<VmResult, VmError> {
        if self.stack.is_empty() {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        let fsize = proto.max_stack_size as usize;
        self.code = proto.code.clone();
        self.constants = proto.constants.clone();
        self.upval_descs = proto.upvalues.clone();
        self.protos = proto.protos.clone();
        self.base = 0;
        self.pc = 0;
        self.num_params = proto.num_params;
        self.is_vararg = proto.is_vararg();
        self.proto_flag = proto.flag;
        self.nextraargs = 0;
        self.closure_upvals = Vec::new();
        self.tbc_list = None;
        self.open_upval = None;

        while self.stack.len() < fsize {
            self.stack.push(TValue::Nil(NilKind::Strict));
        }
        VmExecutor::execute_loop(self)
    }

    /// 从 Proto 构建执行上下文（原 VmState::new）
    ///
    /// 函数帧布局: stack[base-1] = 函数入口, stack[base+0..base+N] = 寄存器/参数
    /// 当 base=0 时，stack[0] 兼作函数入口和寄存器 0（主函数场景）
    pub fn from_proto(proto: &Proto, base: usize, mut stack: Vec<TValue>, gc: Rc<GCState>) -> Self {
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
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };

        let registry = {
            let t = Table::new();
            let id = gc.register_object(64);
            t.gc_header.set_id(id);
            t
        };

        LuaState {
            constants: proto.constants.clone(),
            code: proto.code.clone(),
            upval_descs: proto.upvalues.clone(),
            protos: proto.protos.clone(),
            top,
            base,
            pc: 0,
            trap: false,
            num_params: proto.num_params,
            is_vararg: proto.is_vararg(),
            proto_flag: proto.flag,
            nextraargs: 0,
            closure_upvals: Vec::new(),
            open_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
            stack,
            gc,
            globals,
            registry,
            string_table: StringTable::new(),
            api_func_base: 0,
            n_ccalls: 0,
            n_ny_calls: 0,
            dmt: DefaultMetatables::new(),
            stdout: Box::new(std::io::stdout()),
            io_output: None,
            file_handles: std::collections::HashMap::new(),
            popen_handles: std::collections::HashSet::new(),
            io_input_handle: None,
            io_output_handle: None,
            global_state: Rc::new(GlobalState { gcstopem: false }),
            ci: None,
            call_info: Vec::new(),
            last_traceback: String::new(),
            last_error_msg: String::new(),
            last_error_value: None,
            pending_error: None,
            error_no_prefix: false,
            pending_yield: None,
            last_c_function: None,
            math_random_state: None,
            hook_func: None,
            hook_mask: 0,
            hook_count: 0,
            current_hook_count: 0,
            hook_old_pc: 0,
            allowhook: true,
            warn_on: true,
            warn_pending: false,
            main_thread: LuaThread {
                stack: Vec::new(),
                status: ThreadStatus::OK,
                function: None,
                is_main: true,
                context: Rc::new(RefCell::new(ThreadContext::default())),
            },
            call_stack: Vec::with_capacity(32),
            current_thread: None,
            wrap_coros: Vec::new(),
            caller_gc_stacks: Vec::new(),
            pcall_protection_stack: Vec::new(),
            weak_tables: Vec::new(),
            concat_gc_counter: std::cell::Cell::new(0),
            concat_gc_interval: std::cell::Cell::new(4096),
            finobj_list: Vec::new(),
            ud_finobj_list: Vec::new(),
            gc_closing: false,
            exit_requested: None,
            transferinfo_ftransfer: 0,
            transferinfo_ntransfer: 0,
            pending_return_adjust: None,
            last_error_call_info: None,
            last_close_frame: None,
            close_error_status: None,
            force_noyield_close: false,
        }
    }
}

impl Default for LuaState {
    fn default() -> Self {
        Self::new()
    }
}

impl LuaState {
    /// 调整 C 函数返回值到栈上 — 对应 C 的 luaD_poscall 中的栈调整
    ///
    /// 当 return hook 启用时，不立即 adjust，而是把结果放到栈顶之上，
    /// 设置 pending_return_adjust，由 op_call 在 return hook 后执行 adjust。
    /// 这样 return hook 中的 debug.getlocal 能从栈上读到返回值。
    pub fn adjust_results(&mut self, a: usize, nresults: i32, results: Vec<TValue>) {
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
                for v in &results {
                    self.stack.push(v.clone());
                }
                self.pending_return_adjust = Some((a, nresults, results.len(), first_result_pos));
                return;
            }
        }

        self.stack.truncate(a);
        let n = if nresults < 0 {
            results.len()
        } else {
            nresults as usize
        };
        for i in 0..n {
            if i < results.len() {
                self.stack.push(results[i].clone());
            } else {
                self.stack.push(TValue::Nil(NilKind::Strict));
            }
        }
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
        // 将结果从 first_result_pos 移动到 a（TValue 非 Copy，用 clone 循环）
        // 正向循环安全：a < first_result_pos，写 a+k 不会覆盖尚未读取的源 first_result_pos+k
        if n_actual > 0 && first_result_pos + n_actual <= self.stack.len() {
            for k in 0..n_actual {
                let val = self.stack[first_result_pos + k].clone();
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
                    let val = self.stack[first_result_pos + k].clone();
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
        }
    }
}

// ============================================================================
// 字符串工具
// ============================================================================

pub fn str_to_ls(table: &StringTable, s: &str) -> LuaString {
    crate::strings::new_lstr(table, s)
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if f == 0.0 {
        return "0.0".to_string();
    }
    let s = format!("{:.15}", f);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{}0", s)
    } else {
        s.to_string()
    }
}

// ============================================================================
// 高层 API 方法（原 LuaState）
// ============================================================================

impl LuaState {
    // ====== Stack ======

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

    // ====== Push ======

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
        let ls = str_to_ls(&self.string_table, s);
        self.stack.push(TValue::Str(ls));
    }

    pub fn push_lstring(&mut self, s: &[u8]) {
        let text = String::from_utf8_lossy(s).into_owned();
        let ls = str_to_ls(&self.string_table, &text);
        self.stack.push(TValue::Str(ls));
    }

    pub fn push_value(&mut self, val: TValue) {
        self.stack.push(val);
    }

    pub fn push_light_userdata(&mut self, p: *mut std::ffi::c_void) {
        self.stack.push(TValue::LightUserData(p));
    }

    pub fn push_fstring(&mut self, fmt: &str) {
        self.push_string(fmt);
    }

    pub fn push_lua_value(&mut self, val: &TValue) {
        self.stack.push(val.clone());
    }

    // ====== Access / Type ======

    pub fn obj_at(&self, idx: isize) -> Option<&TValue> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, idx: isize) -> Option<&mut TValue> {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            Some(&mut self.stack[abs - 1])
        } else {
            None
        }
    }

    pub fn get_type(&self, idx: isize) -> LuaType {
        self.obj_at(idx).map(|v| v.ty()).unwrap_or(LuaType::Nil)
    }

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

    /// 返回指定栈位置值的类型名 — 对应 C 的 luaL_typename
    pub fn typename_at(&self, idx: isize) -> &'static str {
        self.typename(self.get_type(idx))
    }

    pub fn to_integer(&self, idx: isize) -> Option<i64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i),
            Some(TValue::Float(f)) => crate::vm::float_to_integer(*f, crate::vm::F2IMode::Eq),
            Some(TValue::Str(s)) => s.as_str().parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn to_number(&self, idx: isize) -> Option<f64> {
        match self.obj_at(idx) {
            Some(TValue::Integer(i)) => Some(*i as f64),
            Some(TValue::Float(f)) => Some(*f),
            Some(TValue::Str(s)) => s.as_str().parse::<f64>().ok(),
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
            Some(TValue::Str(s)) => Some(s.as_str().to_string()),
            Some(TValue::Integer(i)) => Some(i.to_string()),
            Some(TValue::Float(f)) => Some(format_float(*f)),
            _ => None,
        }
    }

    pub fn to_lstring(&self, idx: isize) -> Option<(String, usize)> {
        match self.obj_at(idx) {
            Some(TValue::Str(s)) => {
                let text = s.as_str().to_string();
                let len = s.len();
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

    // ====== Globals ======

    pub fn get_global(&mut self, name: &str) -> LuaType {
        let key = TValue::Str(str_to_ls(&self.string_table, name));
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
        let key = TValue::Str(str_to_ls(&self.string_table, name));
        if let Some(val) = self.stack.pop() {
            self.globals.set(key, val);
        }
    }

    pub fn set_field(&mut self, idx: isize, key_name: &str) {
        let abs = self.abs_index(idx);
        let val = self.stack.pop().unwrap_or(TValue::Nil(NilKind::Strict));
        let key = TValue::Str(str_to_ls(&self.string_table, key_name));
        if abs > 0 && abs <= self.stack.len() {
            let tbl = &mut self.stack[abs - 1];
            if let TValue::Table(ref mut t) = tbl {
                t.set(key, val);
            }
        }
    }

    pub fn get_field(&mut self, idx: isize, key_name: &str) -> LuaType {
        let abs = self.abs_index(idx);
        let key = TValue::Str(str_to_ls(&self.string_table, key_name));
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

    // ====== Table ======

    pub fn create_table(&mut self, narr: usize, nrec: usize) {
        let t = Table::with_capacity(narr, nrec);
        self.stack.push(TValue::Table(t));
    }

    pub fn new_table(&mut self) {
        let t = Table::new();
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

    // ====== Len ======

    pub fn len(&self, idx: isize) -> usize {
        let abs = self.abs_index(idx);
        if abs > 0 && abs <= self.stack.len() {
            match &self.stack[abs - 1] {
                TValue::Table(t) => t.len() as usize,
                TValue::Str(s) => s.len(),
                TValue::Integer(_) | TValue::Float(_) => 0,
                _ => 0,
            }
        } else {
            0
        }
    }

    // ====== Check stack ======

    pub fn check_stack(&mut self, extra: usize) -> bool {
        let needed = self.stack.len() + extra;
        if needed > self.stack.capacity() {
            self.stack.reserve(extra);
        }
        true
    }

    // ====== Garbage Collection ======

    pub fn gc_stop(&self) {
        self.gc.gc_stop.set(1);
    }

    pub fn gc_restart(&self) {
        self.gc.gc_stop.set(0);
    }

    pub fn gc_gen(&self) {
        self.gc.set_mode(crate::gc::GCMode::Generational);
    }

    pub fn gc_inc(&self) {
        self.gc.set_mode(crate::gc::GCMode::Incremental);
    }

    /// 关闭状态：调用所有注册了 __gc 的对象的 finalizer（不检查可达性）。
    /// 对应 C 的 close_state → luaC_freeallobjects → separatetobefnz(1) + callallpendingfinalizers。
    /// finobj_list 末尾是后创建的对象，按逆序调用（后创建先调用）。
    pub fn close_state(&mut self) {
        // 已在关闭中：finalizer 中调用 os.exit(close=true) 重入，不再处理
        if self.gc_closing {
            return;
        }
        self.gc_closing = true;
        // 关闭所有 to-be-closed 变量（对应 C 的 luaD_closeprotected(L, 1, LUA_OK)）
        // C 版 close_state 调用 luaD_closeprotected(L, 1, LUA_OK) 关闭从栈位置 1 开始的
        // 所有 upvalues，触发 TBC 变量的 __close 元方法。忽略错误继续关闭。
        let _ = crate::func::close(self, 1, 0, 0);
        // 按创建逆序调用 finalizer（后创建的先调用）
        let mut to_finalize: Vec<TValue> = Vec::new();
        for t in self.finobj_list.drain(..).rev() {
            to_finalize.push(TValue::Table(t));
        }
        for u in self.ud_finobj_list.drain(..).rev() {
            to_finalize.push(TValue::UserData(Box::new(u)));
        }
        self.call_finalizers(to_finalize);
        // finalizer 中可能调用 os.exit(code, true) 设置 exit_requested
        if let Some(code) = self.exit_requested {
            std::process::exit(code);
        }
    }

    // ====== Diagnostics ======

    pub fn warning(&mut self, msg: &str, tocont: bool) {
        use std::io::Write;
        if !self.warn_pending {
            // warnfon / warnfoff 行为：检查控制消息
            if !tocont && msg.starts_with('@') {
                let ctl = &msg[1..];
                match ctl {
                    "off" => {
                        self.warn_on = false;
                        self.warn_pending = false;
                    }
                    "on" => {
                        self.warn_on = true;
                        self.warn_pending = false;
                    }
                    _ => {} // 未知控制消息，忽略
                }
                return;
            }
            if !self.warn_on {
                return;
            }
            // warnfon: 输出前缀 + 消息
            let stderr = std::io::stderr();
            let mut h = stderr.lock();
            let _ = write!(h, "Lua warning: ");
            let _ = h.write_all(msg.as_bytes());
            if tocont {
                self.warn_pending = true;
            } else {
                let _ = h.write_all(b"\n");
                self.warn_pending = false;
            }
            let _ = h.flush();
        } else {
            // warnfcont 行为：不检查控制消息，直接输出
            let stderr = std::io::stderr();
            let mut h = stderr.lock();
            let _ = h.write_all(msg.as_bytes());
            if tocont {
                self.warn_pending = true;
            } else {
                let _ = h.write_all(b"\n");
                self.warn_pending = false;
            }
            let _ = h.flush();
        }
    }

    pub fn check_version(&self) {}

    // ====== Call Meta ======

    pub fn call_meta(&self, _idx: isize, _event: &str) -> bool {
        false
    }

    pub fn traceback(&mut self, msg: &str, _level: usize) {
        let trace = format!("stack traceback:\n\t...\n{}", msg);
        self.push_string(&trace);
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
                result.push('\n');
                result.push('\t');
                if entry.is_c {
                    result.push_str("[C]: in ");
                    result.push_str(&entry.name);
                } else {
                    if entry.line > 0 {
                        result.push_str(&format!("{}:{}: in ", entry.source, entry.line));
                    } else {
                        result.push_str(&format!("{}: in ", entry.source));
                    }
                    result.push_str(&entry.name);
                }
            }
            // 最后添加 [C]: in ?
            result.push_str("\n\t[C]: in ?");
        }
        result
    }

    /// 推入调用栈条目 — 用于堆栈回溯
    pub fn push_call_info(&mut self, entry: CallInfoEntry) {
        self.call_info.push(entry);
    }

    /// 弹出调用栈条目
    pub fn pop_call_info(&mut self) {
        self.call_info.pop();
    }

    pub fn error(&mut self, msg: &str) -> String {
        msg.to_string()
    }

    // ====== Push C Function ======

    pub fn push_rust_fn(&mut self, _f: fn(&mut LuaState) -> i32, tag: usize) {
        self.push_light_userdata(tag as *mut std::ffi::c_void);
    }

    // ====== Load Code ======

    pub fn load_buffer(&mut self, code: &str, chunk_name: &str) -> i32 {
        // 使用阈值触发 GC 而非强制完整 GC — 避免每个 load() 调用都做 O(objects) 标记。
        // 当 gc_estimate 超过 collect_threshold 时自动收集，否则跳过。
        // 这对 construct.lua（206,780 次 load() 调用）是关键的优化：
        // 非强制 GC 路径节省了完整标记遍历的 all-objects 扫描开销。
        self.maybe_collect_gc();
        match crate::compiler::compile(self, code, chunk_name) {
            Ok(proto) => {
                let nup = proto.size_upvalues as usize;
                let mut upvals: Vec<UpValRef> = Vec::with_capacity(nup.max(1));
                upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                    value: Box::new(TValue::Table(self.globals.clone())),
                })));
                for _ in 1..nup {
                    upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                        value: Box::new(TValue::Nil(NilKind::Strict)),
                    })));
                }
                let closure = Box::new(LClosure {
                    gc_header: GCObjectHeader::new(),
                    proto: Rc::new(proto),
                    upvals: Rc::new(RefCell::new(upvals)),
                });
                self.stack.push(TValue::LClosure(closure));
                0
            }
            Err(err_msg) => {
                self.push_string(&err_msg);
                ERR_SYNTAX
            }
        }
    }

    /// 对应 C 的 `luaL_loadfilex`：从文件或 stdin 加载 Lua 代码。
    ///
    /// - `filename` 为 `Some(path)` 时读取文件；为 `None` 时读取 stdin。
    /// - `mode` 用于控制是否允许文本/二进制块（当前仅文本块可实际加载）。
    ///
    /// 成功时返回 0，并将主闭包压入栈顶；失败时压入错误信息并返回错误码。
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

    pub fn load_file(&mut self, fname: Option<&str>) -> i32 {
        self.load_filex(fname, None)
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
            return match crate::compiler::bytecode_dump::undump_to_proto(rest) {
                Ok(mut proto) => {
                    // 驻留化字符串 (LongString → ShortString)
                    crate::stdlib::base_lib::intern_proto_strings(&mut proto, self);
                    let nup = proto.size_upvalues as usize;
                    let mut upvals: Vec<UpValRef> = Vec::with_capacity(nup.max(1));
                    upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                        value: Box::new(TValue::Table(self.globals.clone())),
                    })));
                    for _ in 1..nup {
                        upvals.push(Rc::new(RefCell::new(UpVal::Closed {
                            value: Box::new(TValue::Nil(NilKind::Strict)),
                        })));
                    }
                    let closure = Box::new(LClosure {
                        gc_header: GCObjectHeader::new(),
                        proto: Rc::new(proto),
                        upvals: Rc::new(RefCell::new(upvals)),
                    });
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
        self.ccall(nargs, nresults, nyci)
    }

    // ====== pcall ======

    pub fn pcall(&mut self, nargs: usize, nresults: i32, _errfunc: isize) -> i32 {
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
            // 检查是否是 coroutine.wrap 返回的 Table（GC 跟踪，可被回收）
            if let Some(idx) = crate::stdlib::coroutine_lib::get_wrap_idx(&cur_val) {
                let tag = crate::stdlib::coroutine_lib::CORO_WRAP_CALL_BASE + idx;
                let nargs = self.stack.len().saturating_sub(func_idx + 1);
                let result = crate::stdlib::coroutine_lib::call_wrap_call(
                    tag, self, func_idx, nargs, nresults,
                );
                return match result {
                    Ok(()) => 0,
                    Err(e) => {
                        self.stack.truncate(func_idx);
                        let err_val = match e {
                            crate::execute::VmError::RuntimeErrorValue(val) => val,
                            crate::execute::VmError::RuntimeError(s) => {
                                TValue::Str(self.intern_str(&s))
                            }
                            other => TValue::Str(self.intern_str(&format!("{}", other))),
                        };
                        self.last_error_value = Some(err_val.clone());
                        self.stack.push(err_val);
                        ERR_RUN
                    }
                };
            }
            if let TValue::Table(ref t) = cur_val {
                let mt_opt = t.get_metatable();
                let call_fn = mt_opt.as_ref().and_then(|mt| {
                    let call_key = TValue::Str(self.intern_str("__call"));
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
                self.push_string(&format!(
                    "attempt to call a {} value",
                    self.typename(cur_val.ty())
                ));
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

                let saved_code = std::mem::take(&mut self.code);
                let saved_constants = std::mem::take(&mut self.constants);
                let saved_upval_descs = std::mem::take(&mut self.upval_descs);
                let saved_protos = std::mem::take(&mut self.protos);
                let saved_base = self.base;
                let saved_pc = self.pc;
                let saved_num_params = self.num_params;
                let saved_is_vararg = self.is_vararg;
                let saved_proto_flag = self.proto_flag;
                let saved_nextraargs = self.nextraargs;
                let saved_closure_upvals = std::mem::take(&mut self.closure_upvals);
                let saved_tbc_list = self.tbc_list.take();

                // 推入 call_info — 对应 C 的 luaD_precall 创建新 CallInfo
                // state.pcall 不是通过 OP_CALL 调用的（从 C 函数内部调用），
                // 不能用 get_func_name 获取函数名（state.pc 指向调用 pcall/xpcall
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
                        .map_or(false, |c| Rc::ptr_eq(&c.proto, &closure.proto))
                        || (!e.namewhat.is_empty() && !e.is_c)
                });
                let pushed_call_info = !already_has_entry;
                if pushed_call_info {
                    let (caller_source, caller_line, caller_base, caller_pc) = if caller_is_lua {
                        if let TValue::LClosure(prev_closure) = &self.stack[self.base - 1] {
                            let src = prev_closure
                                .proto
                                .source
                                .as_ref()
                                .map(|s| s.as_str().to_string())
                                .unwrap_or_else(|| "=?".to_string());
                            let line = crate::execute::get_proto_line(&prev_closure.proto, self.pc);
                            (src, line, self.base, self.pc)
                        } else {
                            unreachable!()
                        }
                    } else {
                        // C 调用者: source="=[C]", line=-1, base=0, saved_pc=0
                        ("=[C]".to_string(), -1, 0usize, 0usize)
                    };
                    self.call_info.push(crate::state::CallInfoEntry {
                        source: caller_source,
                        line: caller_line,
                        name: String::new(),
                        is_c: !caller_is_lua,
                        closure: Some(closure.clone()),
                        base: caller_base,
                        saved_pc: caller_pc,
                        namewhat: String::new(),
                        proto_flag: self.proto_flag,
                        nextraargs: self.nextraargs,
                        is_tailcall: false,
                    });
                }

                self.code = closure.proto.code.clone();
                self.constants = closure.proto.constants.clone();
                self.upval_descs = closure.proto.upvalues.clone();
                self.protos = closure.proto.protos.clone();
                self.base = func_idx + 1;
                self.pc = 0;
                self.num_params = closure.proto.num_params;
                self.is_vararg = closure.proto.is_vararg();
                self.proto_flag = closure.proto.flag;
                self.nextraargs = 0;
                // 关键: 将闭包的上值转移到 state，供 GETUPVAL/SETUPVAL 使用
                // upvals 是 Rc<RefCell<Vec>>，这里 clone 出 Vec 供执行期使用
                self.closure_upvals = closure.upvals.borrow().clone();
                self.tbc_list = None;
                // 不清空 state.open_upval: 全局链表机制下，open_upval 不随函数调用/返回保存/恢复
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
                } else {
                    let frame_end = func_idx + 1 + fsize;
                    while self.stack.len() < frame_end {
                        self.stack.push(TValue::Nil(NilKind::Strict));
                    }
                    for i in nargs_actual..nfixparams {
                        self.stack[func_idx + 1 + i] = TValue::Nil(NilKind::Strict);
                    }
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
                            saved_code: Vec::new(),
                            saved_constants: Vec::new(),
                            saved_upval_descs: Vec::new(),
                            saved_protos: Vec::new(),
                            saved_base: 0,
                            saved_pc: 0,
                            saved_num_params: 0,
                            saved_is_vararg: false,
                            saved_proto_flag: 0,
                            saved_nextraargs: 0,
                            saved_closure_upvals: Vec::new(),
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
                // yield 路径: execute_loop 返回 Yield，call_stack 可能有残留（被中断的调用），
                //   保留这些帧供协程恢复时使用。
                if !matches!(&result, Ok(VmResult::Yield { .. })) {
                    self.call_stack = saved_call_stack_frames;
                } else {
                    // yield: 合并残留帧（如果有）到 saved 帧之前
                    // 通常 yield 时 call_stack 包含被中断的调用帧
                    let mut remaining = std::mem::take(&mut self.call_stack);
                    remaining.extend(saved_call_stack_frames);
                    self.call_stack = remaining;
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
                        top.saved_upval_descs = saved_upval_descs.clone();
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
                        top.saved_closure_upvals = saved_closure_upvals.clone();
                        top.saved_tbc_list = saved_tbc_list;
                        top.func_idx = func_idx;
                        // 不覆盖 nresults: PcallProtection.nresults 保存的是
                        // call_pcall/call_xpcall 的 nresults (pcall 调用者期望的返回值数),
                        // 供 finish_pcall_return continuation 使用。
                        top.saved_filled = true;
                    }
                }

                // 错误时关闭 to-be-closed 变量（对应 C 的 luaD_closeprotected -> luaF_close）
                // 必须在恢复 saved 状态之前调用，此时 state.closure_upvals/open_upval 仍是 foo 的
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
                                VmError::RuntimeError(s) => TValue::Str(self.intern_str(s)),
                                other => TValue::Str(self.intern_str(&format!("{}", other))),
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
                                                         // call_close_method 推入的 __close CallInfoEntry 的 base = state.base
                                                         // = close_level。state.stack[close_level-1] 是 foo 闭包（LClosure），
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
                        top.saved_upval_descs = saved_upval_descs.clone();
                        top.saved_protos = saved_protos.clone();
                        top.saved_base = saved_base;
                        top.saved_pc = saved_pc + 1;
                        top.saved_num_params = saved_num_params;
                        top.saved_is_vararg = saved_is_vararg;
                        top.saved_proto_flag = saved_proto_flag;
                        top.saved_nextraargs = saved_nextraargs;
                        top.saved_closure_upvals = saved_closure_upvals.clone();
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
                    self.upval_descs = saved_upval_descs;
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
                                tmp_results.push(std::mem::take(&mut self.stack[result_base + i]));
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
                            self.stack.truncate(func_idx);
                            // close 后 last_error_value 包含最终错误值（可能是原始错误或 __close 错误）
                            // 保留原始 TValue 类型（如数字 43），pcall 返回 (false, err_value)
                            let err_val = close_err.unwrap_or_else(|| TValue::Nil(NilKind::Strict));
                            // 对字符串错误，使用 build_traceback 添加了 source:line 前缀的
                            // last_error_msg（VM 错误如 "attempt to call" 需要前缀；
                            // error()/assert() 的前缀已在 last_error_value 中，二者一致）
                            let err_val = if matches!(err_val, TValue::Str(_))
                                && !self.last_error_msg.is_empty()
                            {
                                TValue::Str(self.intern_str(&self.last_error_msg))
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
            TValue::LightUserData(tag) => {
                let tag_val = tag as usize;
                let nargs = self.stack.len().saturating_sub(func_idx + 1);

                // 推入 CallInfoEntry，让 debug.getinfo/traceback 能正确看到 C 函数帧
                // 对应 C 的 luaD_precall -> inc_ci 创建新的 CallInfo
                let c_func_name: Option<String> = if crate::stdlib::base_lib::is_base_tag(tag_val) {
                    crate::stdlib::base_lib::base_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::debug_lib::is_debug_tag(tag_val) {
                    crate::stdlib::debug_lib::debug_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                    crate::stdlib::math_lib::math_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                    crate::stdlib::utf8_lib::utf8_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::table_lib::is_table_tag(tag_val) {
                    crate::stdlib::table_lib::table_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::os_lib::is_os_tag(tag_val) {
                    crate::stdlib::os_lib::os_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                    crate::stdlib::coroutine_lib::coro_function_name(tag_val).map(|s| s.to_string())
                } else if crate::stdlib::io_lib::is_io_function_tag(tag_val) {
                    crate::stdlib::io_lib::io_function_name(tag_val).map(|s| s.to_string())
                } else if tag_val >= 100 {
                    crate::stdlib::string_lib::string_function_name(tag_val).map(|s| s.to_string())
                } else {
                    None
                };
                self.call_info.push(crate::state::CallInfoEntry {
                    source: "=[C]".to_string(),
                    line: -1,
                    name: c_func_name.unwrap_or_default(),
                    is_c: true,
                    closure: None,
                    base: func_idx + 1,
                    saved_pc: self.pc,
                    namewhat: "field".to_string(),
                    proto_flag: self.proto_flag,
                    nextraargs: self.nextraargs,
                    is_tailcall: false,
                });

                // 派发 C 函数并收集结果
                let dispatch_result: Result<(), crate::execute::VmError> =
                    if crate::stdlib::base_lib::is_base_tag(tag_val) {
                        crate::stdlib::base_lib::call_base_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::math_lib::is_math_tag(tag_val) {
                        crate::stdlib::math_lib::call_math_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::utf8_lib::is_utf8_tag(tag_val) {
                        crate::stdlib::utf8_lib::call_utf8_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::table_lib::is_table_tag(tag_val) {
                        crate::stdlib::table_lib::call_table_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::debug_lib::is_debug_tag(tag_val) {
                        crate::stdlib::debug_lib::call_debug_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::os_lib::is_os_tag(tag_val) {
                        crate::stdlib::os_lib::call_os_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::coroutine_lib::is_coro_tag(tag_val) {
                        crate::stdlib::coroutine_lib::call_coro_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::io_lib::is_io_function_tag(tag_val) {
                        crate::stdlib::io_lib::call_io_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::io_lib::is_lines_iterator_tag(tag_val) {
                        // io.lines/file:lines 返回的迭代器
                        crate::stdlib::io_lib::call_lines_iterator(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if crate::stdlib::coroutine_lib::is_wrap_call_tag(tag_val) {
                        crate::stdlib::coroutine_lib::call_wrap_call(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else if tag_val >= 100 {
                        crate::stdlib::string_lib::call_string_function(
                            tag_val, self, func_idx, nargs, nresults,
                        )
                    } else {
                        Err(crate::execute::VmError::RuntimeError(format!(
                            "attempt to call a non-function value (tag={})",
                            tag_val
                        )))
                    };

                // 弹出 C 函数的 CallInfoEntry
                self.call_info.pop();

                match dispatch_result {
                    Ok(()) => 0,
                    // yield: C 函数 (如 call_pcall/call_xpcall) 传播的 yield
                    // 对应 C 中 yield 通过 longjmp 穿过 pcall:
                    // 不截断栈,保留被调用函数的执行状态供协程恢复时使用
                    // (与 LClosure 分支的 yield 处理一致)
                    Err(crate::execute::VmError::Yield(values)) => {
                        self.pending_yield = Some(values);
                        self.last_error_value = None;
                        self.last_error_msg.clear();
                        // C 函数 __close (如 coroutine.yield 作为 __close) yield 时，
                        // state.code/state.pc 未被修改（仍为 close 调用者的上下文）。
                        // 需要更新 close continuation 的 PcallProtection，
                        // 以便 resume 时 finish_close_continuation 能正确恢复并重新执行 OP_RETURN/OP_CLOSE。
                        // (与 LClosure 分支的 yield 处理一致)
                        let pp_len = self.pcall_protection_stack.len();
                        let target_idx = (0..pp_len).rev().find(|&i| {
                            !self.pcall_protection_stack[i].saved_filled
                                && self.pcall_protection_stack[i].is_close_continuation
                        });
                        if let Some(idx) = target_idx {
                            let top = &mut self.pcall_protection_stack[idx];
                            top.saved_code = self.code.clone();
                            top.saved_constants = self.constants.clone();
                            top.saved_upval_descs = self.upval_descs.clone();
                            top.saved_protos = self.protos.clone();
                            top.saved_base = self.base;
                            // is_close_continuation: saved_pc 不 +1，保留指向 OP_RETURN/OP_CLOSE
                            top.saved_pc = self.pc;
                            top.saved_num_params = self.num_params;
                            top.saved_is_vararg = self.is_vararg;
                            top.saved_proto_flag = self.proto_flag;
                            top.saved_nextraargs = self.nextraargs;
                            top.saved_closure_upvals = self.closure_upvals.clone();
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
            _ => {
                self.stack.truncate(func_idx);
                self.push_string(&format!(
                    "attempt to call a {} value",
                    self.typename(func_val.ty())
                ));
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

        // 预留 capacity，但不 push nil（push 会改变 stack.len()，导致 C 函数中
        // lua_gettop 返回值偏移）。C 函数需要空间时通过 lua_checkstack 或
        // lua_pushxxx 自动扩展栈。
        let needed_cap = self.stack.len() + MIN_STACK;
        if self.stack.capacity() < needed_cap {
            self.stack.reserve(MIN_STACK);
        }

        // 清空 pending_error，捕获 C 函数中 lua_error 触发的 panic
        // 对应 C 的 longjmp 跨 C 函数抛错到 pcall 的 setjmp
        self.pending_error = None;
        let ptr: *mut LuaState = self;
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            f(ptr as *mut c_void)
        }));

        // 恢复 api_func_base 和 n_ccalls（无论成功失败）
        self.api_func_base = saved_api_base;
        self.n_ccalls = self.n_ccalls.saturating_sub(1);

        // 错误路径：lua_error 触发 panic + pending_error
        if let Err(payload) = panic_result {
            if let Some(err_val) = self.pending_error.take() {
                self.stack.truncate(func_idx);
                self.stack.push(err_val);
                return ERR_RUN;
            }
            // 非 lua_error 触发的 panic，re-panic 保持原行为
            std::panic::resume_unwind(payload);
        }

        let n = panic_result.unwrap();
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

    // ====== Open Libs ======

    pub fn open_selected_libs(&mut self, _mask: i32, _ignored: i32) {
        let arg_table = Table::new();
        self.globals.set(
            TValue::Str(str_to_ls(&self.string_table, "arg")),
            TValue::Table(arg_table),
        );

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
            let name_val = TValue::Str(self.intern_str(name));
            if let Some(lib) = self.globals.get(&name_val) {
                let package_key = TValue::Str(self.intern_str("package"));
                if let Some(TValue::Table(pkg)) = self.globals.get(&package_key) {
                    let loaded_key = TValue::Str(self.intern_str("loaded"));
                    if let Some(TValue::Table(loaded)) = pkg.get(&loaded_key) {
                        loaded.set(name_val, lib);
                    }
                }
            }
        }
    }

    // ====== Hook ======

    pub fn set_hook(&mut self, _hook: Option<(usize, usize)>, _mask: i32, _count: i32) {}

    // ====== String Helpers ======

    pub fn intern_str(&self, s: &str) -> LuaString {
        str_to_ls(&self.string_table, s)
    }

    pub fn intern(&self, s: &str) -> LuaString {
        str_to_ls(&self.string_table, s)
    }

    // ====== 弱引用表管理 ======

    /// 注册弱引用表 — 当 setmetatable 设置包含 __mode 的元表时调用
    /// 使用 Weak 引用避免阻止表本身的回收
    pub fn register_weak_table(&mut self, t: &Table) {
        self.weak_tables.push(std::rc::Rc::downgrade(&t.data));
    }

    /// 注册有 __gc 元方法的对象 — 当 setmetatable 设置包含 __gc 的元表时调用
    pub fn register_finobj(&mut self, t: &Table) {
        if self.gc_closing {
            return;
        }
        let ptr_id = t.gc_header.ptr_id;
        if !self
            .finobj_list
            .iter()
            .any(|x| x.gc_header.ptr_id == ptr_id)
        {
            self.finobj_list.push(t.clone());
        }
    }

    /// 注册有 __gc 元方法的 UserData — FILE* 等通过默认元表设置的 userdata
    pub fn register_ud_finobj(&mut self, u: &crate::objects::Udata) {
        if self.gc_closing {
            return;
        }
        let ptr_id = u.gc_header.ptr_id;
        if !self
            .ud_finobj_list
            .iter()
            .any(|x| x.gc_header.ptr_id == ptr_id)
        {
            self.ud_finobj_list.push(u.clone());
        }
    }

    /// ephemeron 表传递性处理 — 在标记阶段后、清理弱表前调用。
    /// 对应 C Lua 的 traverseephemeron + convergeephemerons。
    /// 对于纯弱键表（__mode = "k"）：
    ///   - 值可达 → 保留键（标记键引用的对象）
    ///   - 键可达 → 标记值（值引用的对象变为可达）
    /// 迭代直到收敛。
    /// 注意：弱键值表（__mode = "kv"）不参与 ephemeron 传递性，其键和值都按弱引用独立判断。
    fn process_ephemerons(
        &self,
        reachable: &mut HashSet<usize>,
        visited: &mut HashSet<usize>,
        worklist: &mut Vec<TValue>,
    ) {
        let mode_key = TValue::Str(self.intern_str("__mode"));
        let mut changed = true;
        while changed {
            changed = false;
            for weak_rc in &self.weak_tables {
                let data_rc = match weak_rc.upgrade() {
                    None => continue,
                    Some(rc) => rc,
                };
                let (weak_k, weak_v) = {
                    let data = data_rc.borrow();
                    match &data.metatable {
                        Some(mt) => match mt.get(&mode_key) {
                            Some(TValue::Str(s)) => {
                                let mode = s.as_str();
                                (mode.contains('k'), mode.contains('v'))
                            }
                            _ => (false, false),
                        },
                        None => (false, false),
                    }
                };
                // 只处理纯弱键表（ephemeron 表）：weak_k && !weak_v
                // 弱键值表的键和值都按弱引用独立判断，不参与 ephemeron 传递性
                if !(weak_k && !weak_v) {
                    continue;
                }
                let data = data_rc.borrow();
                for (k, v) in data.hash_buckets.iter() {
                    let k_reachable = Self::is_marked(k, reachable);
                    let v_reachable = Self::is_marked(v, reachable);
                    if v_reachable && !k_reachable {
                        let v_is_gc = matches!(
                            v,
                            TValue::Table(_) | TValue::LClosure(_) | TValue::UserData(_)
                        );
                        if v_is_gc {
                            let k_id = match k {
                                TValue::Table(t) => t.gc_header.id(),
                                TValue::LClosure(c) => c.gc_header.id(),
                                TValue::UserData(u) => u.gc_header.id(),
                                _ => None,
                            };
                            if let Some(id) = k_id {
                                reachable.insert(id.0 as usize);
                            }
                            worklist.push(k.clone());
                            changed = true;
                        }
                    }
                    if k_reachable && !v_reachable {
                        worklist.push(v.clone());
                        changed = true;
                    }
                }
            }
            while let Some(val) = worklist.pop() {
                self.mark_tvalue(&val, reachable, visited, worklist);
            }
        }
    }

    /// 清理弱引用表 — 在 collect_gc 标记完成后、sweep 之前调用
    /// 基于 GC reachable 集合判断键/值是否死亡，清除死亡的弱引用条目
    fn clear_weak_tables(&mut self, reachable: &HashSet<usize>) {
        let mode_key = TValue::Str(self.intern_str("__mode"));
        let mut i = 0;
        while i < self.weak_tables.len() {
            match self.weak_tables[i].upgrade() {
                None => {
                    self.weak_tables.swap_remove(i);
                }
                Some(data_rc) => {
                    let (weak_k, weak_v) = {
                        let data = data_rc.borrow();
                        match &data.metatable {
                            Some(mt) => match mt.get(&mode_key) {
                                Some(TValue::Str(s)) => {
                                    let mode = s.as_str();
                                    (mode.contains('k'), mode.contains('v'))
                                }
                                _ => (false, false),
                            },
                            None => (false, false),
                        }
                    };
                    if weak_k || weak_v {
                        let mut to_clear_array: Vec<usize> = Vec::new();
                        let mut to_clear_hash: Vec<TValue> = Vec::new();
                        {
                            let data = data_rc.borrow();
                            if weak_v {
                                for (idx, val) in data.array.iter().enumerate() {
                                    if matches!(val, TValue::Nil(NilKind::Empty)) {
                                        continue;
                                    }
                                    let marked = Self::is_marked(val, reachable);
                                    if !marked {
                                        to_clear_array.push(idx);
                                    }
                                }
                            }
                            for (k, v) in data.hash_buckets.iter() {
                                let k_dead = weak_k && !Self::is_marked(k, reachable);
                                let v_dead = weak_v && !Self::is_marked(v, reachable);
                                let remove = if weak_k && weak_v {
                                    k_dead || v_dead
                                } else if weak_k {
                                    k_dead
                                } else {
                                    v_dead
                                };
                                if remove {
                                    to_clear_hash.push(k.clone());
                                }
                            }
                        }
                        if !to_clear_array.is_empty() || !to_clear_hash.is_empty() {
                            let mut data = data_rc.borrow_mut();
                            for idx in to_clear_array {
                                data.array[idx] = TValue::Nil(NilKind::Empty);
                            }
                            for k in to_clear_hash {
                                if let Some(i) =
                                    data.key_to_bucket.as_mut().and_then(|m| m.remove(&k))
                                {
                                    let last_idx = data.hash_buckets.len() - 1;
                                    if i != last_idx {
                                        // 把最后一个条目移到空隙，保持连续性
                                        let key = data.hash_buckets.last().unwrap().0.clone();
                                        data.hash_buckets.swap_remove(i);
                                        data.key_to_bucket.as_mut().unwrap().insert(key, i);
                                    } else {
                                        data.hash_buckets.pop();
                                    }
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }
        }
    }

    /// 判断值是否被 GC 标记为存活（在 reachable 集合中）
    /// 非 GC 对象（字符串、数字、布尔、nil）总是视为存活
    fn is_marked(val: &TValue, reachable: &HashSet<usize>) -> bool {
        match val {
            TValue::Table(t) => t
                .gc_header
                .id()
                .map_or(true, |id| reachable.contains(&(id.0 as usize))),
            TValue::LClosure(c) => c
                .gc_header
                .id()
                .map_or(true, |id| reachable.contains(&(id.0 as usize))),
            TValue::UserData(u) => u
                .gc_header
                .id()
                .map_or(true, |id| reachable.contains(&(id.0 as usize))),
            _ => true,
        }
    }

    // ========================================================================
    // Mark-Sweep GC — 从根集合标记所有可达对象，然后清扫不可达对象
    // ========================================================================

    /// 完整的 mark-sweep GC：从根集合开始标记所有可达对象，然后清扫不可达对象。
    /// 对应 C 的 luaC_fullgc。
    pub fn collect_gc(&mut self) {
        // 设置 GC 正在运行标志 — 阻止 finalizer 中重入 collectgarbage("collect")
        self.gc.gc_running.set(true);
        // 临时禁用 hook — GC 期间的 finalizer 调用不应触发用户的 hook
        // （对应 C 的 luaD_rawrunprotected 中 L->allowhook = 0 语义）
        let saved_allowhook = self.allowhook;
        self.allowhook = false;

        let result = self.collect_gc_inner();

        // 恢复 hook 状态
        self.allowhook = saved_allowhook;
        // 清除标志
        self.gc.gc_running.set(false);
        // GC 回收对象后，Rust 分配器（glibc malloc）可能仍持有释放的内存不归还操作系统。
        // malloc_trim(0) 强制分配器将所有未使用的堆内存归还操作系统，避免在 200MB
        // 限制下因碎片化导致大分配失败（big.lua 的 48MB Vec 倍增分配）。
        unsafe {
            libc::malloc_trim(0);
        }
        result
    }

    fn collect_gc_inner(&mut self) {
        let mut reachable: HashSet<usize> = HashSet::new();
        let mut visited: HashSet<usize> = HashSet::new();
        let mut worklist: Vec<TValue> = Vec::new();

        // 收集根：栈 — 遍历到 self.top（对应 C Lua 的 traversethread: o < th->top）
        // self.top 在 OP_CALL 中被设为 ra + b（函数+参数末尾），
        // 在 OP_RETURN 中被设为返回值末尾。超出 self.top 的栈槽是"死亡"的。
        let stack_top = self.top.min(self.stack.len());
        for val in &self.stack[..stack_top] {
            worklist.push(val.clone());
        }

        // 收集根：全局表、registry
        worklist.push(TValue::Table(self.globals.clone()));
        worklist.push(TValue::Table(self.registry.clone()));

        // 收集根：closure_upvals
        for uv_ref in &self.closure_upvals {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => {
                    worklist.push((**value).clone());
                }
                UpVal::Open { stack_index, .. } => {
                    if *stack_index < self.stack.len() {
                        worklist.push(self.stack[*stack_index].clone());
                    }
                }
            }
        }

        // 收集根：call_stack 中的 constants 和 closure_upvals
        for frame in &self.call_stack {
            for val in &frame.constants {
                worklist.push(val.clone());
            }
            for uv_ref in &frame.closure_upvals {
                let uv = uv_ref.borrow();
                if let UpVal::Closed { value } = &*uv {
                    worklist.push((**value).clone());
                }
            }
        }

        // 收集根：call_info 中的 closures
        for ci in &self.call_info {
            if let Some(ref closure) = ci.closure {
                worklist.push(TValue::LClosure(closure.clone()));
            }
        }

        // 收集根：hook_func
        if let Some(ref hook) = self.hook_func {
            worklist.push(hook.clone());
        }

        // 收集根：call_wrap_call 期间的调用者栈
        // 嵌套 wrap 调用时，外层协程的栈（含活跃的 wrap table 引用）暂存于此，
        // 否则 GC 看不到内层协程引用，会误判为不可达。
        for stack in &self.caller_gc_stacks {
            for val in stack {
                worklist.push(val.clone());
            }
        }

        // 收集根：协程
        // 不再把所有 wrap_coros 作为根，只通过 TValue::Thread 引用跟踪协程可达性
        // 主线程的 context（saved_stack 等）需要被收集
        self.collect_thread_roots(&self.main_thread, &mut worklist);

        // 处理工作列表
        while let Some(val) = worklist.pop() {
            self.mark_tvalue(&val, &mut reachable, &mut visited, &mut worklist);
        }

        // ephemeron 表传递性处理：对于弱键表，如果值可达，则保留键。
        // 对应 C Lua 的 traverseephemeron + convergeephemerons。
        // 迭代直到收敛：值可达 → 标记键 → 键引用的对象可达 → 可能导致其他值可达...
        self.process_ephemerons(&mut reachable, &mut visited, &mut worklist);

        // 收集需要 finalize 的对象并"复活"它们 — 在 clear_weak_tables 之前调用。
        // 对应 C Lua 的 finalizer 对象"复活"语义：finalizer 执行时对象仍可达，
        // 其引用的对象也应被标记，然后才能清除弱表（避免误清除 finalizer 引用的弱表条目）。
        let to_finalize = self.collect_finalizers(&mut reachable, &mut visited, &mut worklist);

        // 复活后可能引入新的 ephemeron 关系，再次处理 ephemeron 直到收敛
        self.process_ephemerons(&mut reachable, &mut visited, &mut worklist);

        // 清理弱引用表：基于标记结果清除死亡的弱引用键/值
        self.clear_weak_tables(&reachable);

        // 调用 finalizer — 复活的对象已标记，此处执行 __gc 元方法
        // call_finalizers 内部会在 finalizer 调用后同步 closed upvalue 值回主线程栈
        self.call_finalizers(to_finalize);

        // 清扫（sweep_unreachable 会自动减少 gc_estimate）
        self.gc.sweep_unreachable(&reachable);

        // 回收不可达的协程：未被 mark_tvalue 遍历的协程（context 指针不在 visited 中）置为 None。
        // 注意：不能用 retain()，因为会压缩数组改变索引，而 wrap table 元表中的 WRAP_MARKER
        // 记录的是原始 idx，call_wrap_call 通过 idx 索引访问，索引必须稳定。
        for entry in self.wrap_coros.iter_mut() {
            if let Some(ref thread) = entry {
                let ptr = Rc::as_ptr(&thread.context) as usize;
                if !visited.contains(&ptr) {
                    *entry = None;
                }
            }
        }

        // 清理字符串表：移除只有字符串表持有的死字符串
        // 对应 C Lua 的 sweepstrings；字符串不注册到 GC metas，需单独清理
        self.string_table.sweep();

        // 动态阈值 = estimate * pause / 100（与 C 实现一致，基于字节而非对象数）
        let pause = self.gc.get_gc_param(GCState::PARAM_PAUSE).max(1) as usize;
        let new_threshold = self.gc.gc_estimate.get() * pause / 100;
        self.gc.set_collect_threshold(new_threshold);
        self.gc.set_debt(100);
        self.gc.step_accum.set(0);
    }
    /// 增量 GC 步进：累加 siz 工作量，达到当前对象数时触发完整 GC 并返回 true
    pub fn step_gc(&mut self, siz: usize) -> bool {
        // generational 模式下未实现 minor collection，直接做 full collection
        // 以保证 weak table 清除等语义正确（对应 C Lua genstep 中的 youngcollection + cleartable）
        if self.gc.current_mode() == crate::gc::GCMode::Generational {
            self.collect_gc();
            return true;
        }
        let n = siz.max(1);
        let acc = self.gc.step_accum.get() + n;
        let threshold = self.gc.metas_len().max(1);
        if acc >= threshold {
            self.collect_gc();
            true
        } else {
            self.gc.step_accum.set(acc);
            false
        }
    }

    /// 收集需要 finalize 的对象并"复活"它们 — 在 clear_weak_tables 之前调用
    /// 找到 finobj_list 中不可达且有 __gc 的对象，标记它们为可达（"复活"），
    /// 并把它们的引用对象加入工作列表。返回 to_finalize 列表供 call_finalizers 调用。
    /// 对应 C Lua 的 finalizer 对象"复活"语义：finalizer 执行时对象仍可达。
    /// 注意：此处不保存 __gc 函数引用，因为 clear_weak_tables 可能清除弱值表中的
    /// __gc 函数（当函数本身不可达时）。call_finalizers 会重新检查 __gc 是否存在。
    fn collect_finalizers(
        &mut self,
        reachable: &mut HashSet<usize>,
        visited: &mut HashSet<usize>,
        worklist: &mut Vec<TValue>,
    ) -> Vec<TValue> {
        let gc_key = TValue::Str(self.intern_str("__gc"));
        let mut to_finalize: Vec<TValue> = Vec::new();
        let mut keep: Vec<Table> = Vec::new();

        for t in self.finobj_list.drain(..) {
            let is_reachable = t
                .gc_header
                .id()
                .map_or(false, |id| reachable.contains(&(id.0 as usize)));
            if is_reachable {
                keep.push(t);
                continue;
            }
            let has_gc = {
                let data = t.data.borrow();
                if let Some(ref mt) = data.metatable {
                    mt.get(&gc_key).is_some()
                } else {
                    false
                }
            };
            if has_gc {
                worklist.push(TValue::Table(t.clone()));
                to_finalize.push(TValue::Table(t));
            }
        }
        self.finobj_list = keep;

        let mut ud_keep: Vec<crate::objects::Udata> = Vec::new();
        for u in self.ud_finobj_list.drain(..) {
            let is_reachable = u
                .gc_header
                .id()
                .map_or(false, |id| reachable.contains(&(id.0 as usize)));
            if is_reachable {
                ud_keep.push(u);
                continue;
            }
            let has_gc = {
                if let Some(ref mt) = u.metatable {
                    mt.get(&gc_key).is_some()
                } else {
                    false
                }
            };
            if has_gc {
                worklist.push(TValue::UserData(Box::new(u.clone())));
                to_finalize.push(TValue::UserData(Box::new(u)));
            }
        }
        self.ud_finobj_list = ud_keep;

        while let Some(val) = worklist.pop() {
            self.mark_tvalue(&val, reachable, visited, worklist);
        }

        to_finalize
    }

    /// 调用 finalizer — 对 to_finalize 列表中的每个对象重新检查 __gc 并调用
    /// 在 clear_weak_tables 之后调用：弱值表中的 __gc 函数若不可达已被清除，
    /// 此时再检查 __gc 是否存在，存在才调用（对应 C Lua tryfinalizer 的语义）。
    /// finalizer 调用后同步 closed upvalue 值回主线程栈：协程首次 resume 时
    /// open upvalue 被关闭，finalizer 修改 closed upvalue 的值不会自动同步回
    /// 主线程栈上的原始位置，需要在此手动同步（通过协程的 upval_origins 记录）。
    fn call_finalizers(&mut self, to_finalize: Vec<TValue>) {
        let gc_key = TValue::Str(self.intern_str("__gc"));

        // 构建 upval_origins 映射：UpVal Rc 指针 -> original_stack_index
        // 遍历主线程栈上的所有协程，收集它们的 upval_origins（首次 resume 时记录）
        let mut upval_origins_map: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for val in self.stack.iter() {
            if let TValue::Thread(t) = val {
                let origins = t.context.borrow().upval_origins.clone();
                for (uv_ref, stack_index) in origins {
                    let ptr = Rc::as_ptr(&uv_ref) as usize;
                    upval_origins_map.insert(ptr, stack_index);
                }
            }
        }

        for obj_val in to_finalize {
            let gc_func = match &obj_val {
                TValue::Table(t) => {
                    let data = t.data.borrow();
                    if let Some(ref mt) = data.metatable {
                        mt.get(&gc_key)
                    } else {
                        None
                    }
                }
                TValue::UserData(u) => {
                    if let Some(ref mt) = u.metatable {
                        mt.get(&gc_key)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let gc_func = match gc_func {
                Some(f) => f,
                None => continue,
            };

            // 收集 finalizer 函数的 upvalue 的 Rc 指针，用于后续同步
            let finalizer_uv_ptrs: Vec<usize> = if let TValue::LClosure(c) = &gc_func {
                c.upvals
                    .borrow()
                    .iter()
                    .map(|uv_ref| Rc::as_ptr(uv_ref) as usize)
                    .collect()
            } else {
                Vec::new()
            };

            let stack_base = self.stack.len();
            let saved_base = self.base;
            let saved_pc = self.pc;
            let saved_closure_upvals = self.closure_upvals.clone();
            let saved_proto_flag = self.proto_flag;
            let saved_nextraargs = self.nextraargs;
            self.stack.push(gc_func);
            self.stack.push(obj_val);
            self.top = self.stack.len();

            let caller_source = if self.base > 0 && self.base <= self.stack.len() {
                if let TValue::LClosure(c) = &self.stack[self.base - 1] {
                    c.proto
                        .source
                        .as_ref()
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "=?".to_string())
                } else {
                    "=[C]".to_string()
                }
            } else {
                "=?".to_string()
            };
            self.call_info.push(CallInfoEntry {
                source: caller_source,
                line: -1,
                name: "__gc".to_string(),
                is_c: false,
                closure: None,
                base: self.base,
                saved_pc: self.pc,
                namewhat: "metamethod".to_string(),
                proto_flag: self.proto_flag,
                nextraargs: self.nextraargs,
                is_tailcall: false,
            });

            let status = self.pcall(1, 0, 0);
            // finalizer 中 error 时生成 warning（对应 C 的 luaE_warnerror(L, "__gc")）
            // os.exit(code, true) 在 finalizer 中设置 exit_requested 并用
            // "__exit_requested__" 错误中断 pcall，这是正常退出流程，不生成 warning
            if status != 0 && self.exit_requested.is_none() {
                let msg = match self.stack.last() {
                    Some(TValue::Str(s)) => s.as_str().to_string(),
                    _ => "error object is not a string".to_string(),
                };
                self.warning("error in ", true);
                self.warning("__gc", true);
                self.warning(" (", true);
                self.warning(&msg, true);
                self.warning(")", false);
            }

            self.stack.truncate(stack_base);
            self.call_info.pop();
            self.top = stack_base;
            self.base = saved_base;
            self.pc = saved_pc;
            self.closure_upvals = saved_closure_upvals;
            self.proto_flag = saved_proto_flag;
            self.nextraargs = saved_nextraargs;

            // 同步 finalizer 函数的 closed upvalue 值回主线程栈
            // finalizer 可能修改了 closed upvalue 的值（如 collected = true），
            // 需要同步回主线程栈上的原始位置（upval_origins 记录的 stack_index）
            for uv_ptr in &finalizer_uv_ptrs {
                if let Some(&stack_index) = upval_origins_map.get(uv_ptr) {
                    // 通过 Rc 指针找到对应的 upvalue，读取当前值
                    // 遍历栈上的协程找到该 upvalue 的 Rc 引用
                    let mut found_val = None;
                    'outer: for val in self.stack.iter() {
                        if let TValue::Thread(t) = val {
                            let origins = t.context.borrow().upval_origins.clone();
                            for (uv_ref, idx) in &origins {
                                if Rc::as_ptr(uv_ref) as usize == *uv_ptr {
                                    let uv = uv_ref.borrow();
                                    if let UpVal::Closed { value } = &*uv {
                                        found_val = Some((**value).clone());
                                    }
                                    break 'outer;
                                }
                            }
                        }
                    }
                    if let Some(val) = found_val {
                        if stack_index < self.stack.len() {
                            self.stack[stack_index] = val;
                        }
                    }
                }
            }
        }
    }

    /// 收集协程的根对象
    fn collect_thread_roots(&self, thread: &LuaThread, worklist: &mut Vec<TValue>) {
        for val in &thread.stack {
            worklist.push(val.clone());
        }
        if let Some(ref func) = thread.function {
            worklist.push((**func).clone());
        }
        let ctx = thread.context.borrow();
        for val in &ctx.saved_stack {
            worklist.push(val.clone());
        }
        for val in &ctx.saved_constants {
            worklist.push(val.clone());
        }
        for uv_ref in &ctx.saved_closure_upvals {
            let uv = uv_ref.borrow();
            match &*uv {
                UpVal::Closed { value } => {
                    worklist.push((**value).clone());
                }
                UpVal::Open { stack_index, .. } => {
                    if *stack_index < ctx.saved_stack.len() {
                        worklist.push(ctx.saved_stack[*stack_index].clone());
                    }
                }
            }
        }
        for frame in &ctx.saved_call_stack {
            for val in &frame.constants {
                worklist.push(val.clone());
            }
            for uv_ref in &frame.closure_upvals {
                let uv = uv_ref.borrow();
                if let UpVal::Closed { value } = &*uv {
                    worklist.push((**value).clone());
                }
            }
        }
        if let Some(ref hook) = ctx.saved_hook_func {
            worklist.push(hook.clone());
        }
        if let Some(ref err) = ctx.error_msg {
            worklist.push(err.clone());
        }
        for ci in &ctx.saved_call_info {
            if let Some(ref closure) = ci.closure {
                worklist.push(TValue::LClosure(closure.clone()));
            }
        }
    }

    /// 标记 TValue 可达，并将子对象加入工作列表
    fn mark_tvalue(
        &self,
        val: &TValue,
        reachable: &mut HashSet<usize>,
        visited: &mut HashSet<usize>,
        worklist: &mut Vec<TValue>,
    ) {
        match val {
            TValue::Table(t) => {
                if let Some(id) = t.gc_header.id() {
                    reachable.insert(id.0 as usize);
                }
                let ptr_id = t.gc_header.ptr_id;
                if visited.insert(ptr_id as usize) {
                    let data = t.data.borrow();
                    let (weak_k, weak_v) = match &data.metatable {
                        Some(mt) => {
                            let mode_key = TValue::Str(self.intern_str("__mode"));
                            match mt.get(&mode_key) {
                                Some(TValue::Str(s)) => {
                                    let mode = s.as_str();
                                    (mode.contains('k'), mode.contains('v'))
                                }
                                _ => (false, false),
                            }
                        }
                        None => (false, false),
                    };
                    for v in data.array.iter() {
                        if !weak_v {
                            worklist.push(v.clone());
                        }
                    }
                    for (k, v) in data.hash_buckets.iter() {
                        if !weak_k {
                            worklist.push(k.clone());
                        }
                        if !weak_k && !weak_v {
                            worklist.push(v.clone());
                        }
                    }
                    if let Some(ref mt) = data.metatable {
                        worklist.push(TValue::Table((**mt).clone()));
                    }
                }
            }
            TValue::LClosure(c) => {
                if let Some(id) = c.gc_header.id() {
                    reachable.insert(id.0 as usize);
                }
                let ptr_id = c.gc_header.ptr_id;
                if visited.insert(ptr_id as usize) {
                    let upvals = c.upvals.borrow();
                    for uv_ref in upvals.iter() {
                        let uv = uv_ref.borrow();
                        match &*uv {
                            UpVal::Closed { value } => {
                                worklist.push((**value).clone());
                            }
                            UpVal::Open { stack_index, .. } => {
                                if *stack_index < self.stack.len() {
                                    worklist.push(self.stack[*stack_index].clone());
                                }
                            }
                        }
                    }
                }
            }
            TValue::UserData(u) => {
                if let Some(id) = u.gc_header.id() {
                    reachable.insert(id.0 as usize);
                }
                let ptr_id = u.gc_header.ptr_id;
                if visited.insert(ptr_id as usize) {
                    if let Some(ref mt) = u.metatable {
                        worklist.push(TValue::Table((**mt).clone()));
                    }
                    for uv in &u.user_values {
                        worklist.push(uv.clone());
                    }
                }
            }
            TValue::Thread(t) => {
                let ptr = Rc::as_ptr(&t.context) as usize;
                if visited.insert(ptr) {
                    self.collect_thread_roots(t, worklist);
                }
            }
            _ => {}
        }
    }

    /// 当 metas 大小超过动态阈值时自动触发 GC
    pub fn maybe_collect_gc(&mut self) {
        if self.gc.is_running() && self.gc.gc_estimate.get() > self.gc.collect_threshold() {
            self.collect_gc();
        }
    }

    /// 字符串拼接的 GC 检查：字符串不注册到 GC metas，metas_len 无法反映
    /// 拼接产生的分配压力，因此用计数器限制：每 concat_gc_interval 次 op_concat
    /// 触发一次 collect_gc，清理弱引用表中的死条目。
    pub fn concat_gc_check(&mut self) {
        let cnt = self.concat_gc_counter.get() + 1;
        if cnt >= self.concat_gc_interval.get() {
            self.collect_gc();
            self.concat_gc_counter.set(0);
        } else {
            self.concat_gc_counter.set(cnt);
        }
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

    #[test]
    fn test_stack_init_matches_cpp() {
        // 验证 stack_init: 参考 lstate.cpp L158-169
        let state = LuaState::new();
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

    #[test]
    fn test_stack_init_from_proto() {
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
            constants: vec![],
            code: vec![],
            protos: vec![],
            upvalues: vec![],
            line_info: vec![],
            abs_line_info: vec![],
            loc_vars: vec![],
            source: None,
        };
        let gc = Rc::new(GCState::default_incremental());

        // case 1: base=0, empty stack → main function scenario
        let state = LuaState::from_proto(&proto, 0, vec![], gc.clone());
        assert_eq!(state.base, 0);
        assert_eq!(
            state.stack.len(),
            10,
            "base=0 with max_stack_size=10 must allocate 10 register slots"
        );

        // case 2: base=1, empty stack → called function scenario
        let state = LuaState::from_proto(&proto, 1, vec![], gc.clone());
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
            gc.clone(),
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
    fn test_stack_init_with_gc() {
        let gc = Rc::new(GCState::default_incremental());
        let state = LuaState::with_gc(gc);
        assert_eq!(state.gettop(), 1, "with_gc must also init stack");
        assert_eq!(state.stack.capacity(), BASIC_STACK_SIZE + EXTRA_STACK);
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
    fn test_load_file_decodes_iso8859_strings() {
        let mut state = LuaState::new();
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
    fn test_load_file_skips_shebang_all() {
        let mut state = LuaState::new();
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
    fn test_load_file_missing() {
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
        let status = state.load_filex(Some(path.to_str().unwrap()), Some("t"));
        assert_eq!(status, ERR_SYNTAX);
        let msg = state.to_string(-1).unwrap_or_default();
        assert!(msg.contains("mode is 'text'"), "unexpected error: {}", msg);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_file_mode_binary_rejects_text() {
        let path = write_tmp("text_bin_mode", b"return 42\n");
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
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
        let mut state = LuaState::new();
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
