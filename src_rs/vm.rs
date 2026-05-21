//! Lua 虚拟机核心实现

use crate::objects::{TValue, Instruction, LClosure, LuaThread, Table, ThreadStatus};

/// Lua 虚拟机状态
pub struct LuaVM {
    /// 当前线程
    pub current_thread: LuaThread,
    /// 全局表
    pub globals: Table,
    /// 栈
    pub stack: Vec<TValue>,
    /// 程序计数器
    pub pc: usize,
}

impl LuaVM {
    pub fn new() -> Self {
        LuaVM {
            current_thread: LuaThread {
                stack: Vec::new(),
                status: ThreadStatus::OK,
            },
            globals: Table::new(),
            stack: Vec::with_capacity(20),
            pc: 0,
        }
    }

    /// 执行一条指令
    #[allow(dead_code)]
    pub fn execute_instruction(&mut self, _instruction: &Instruction) -> Result<(), String> {
        // TODO: 实现指令执行逻辑
        unimplemented!()
    }

    /// 调用函数
    #[allow(dead_code)]
    pub fn call_function(&mut self, _function: &LClosure, _args: Vec<TValue>) -> Result<Vec<TValue>, String> {
        // TODO: 实现函数调用逻辑
        unimplemented!()
    }
}