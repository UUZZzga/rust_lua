pub mod lexer;
pub mod compile;
pub mod bytecode_dump;
// cmp_tests 依赖 lua_ffi（调用 C lua 编译并对比字节码），仅在 ffi feature 启用时编译。
#[cfg(all(test, feature = "ffi"))]
mod cmp_tests;

use crate::{objects::Proto, state::LuaState};

pub fn compile(state: &mut LuaState, source: &str, name: &str) -> Result<Proto, String> {
    let mut ls = lexer::LexState::new(state,source, name);
    compile::compile_chunk(&mut ls)
}