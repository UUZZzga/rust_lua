pub mod bytecode_dump;
pub mod compile;
pub mod lexer;
// cmp_tests 依赖 lua_ffi（调用 C lua 编译并对比字节码），仅在 cmp_c feature 启用时编译。
#[cfg(all(test, feature = "cmp_c"))]
mod cmp_tests;

use crate::{objects::Proto, state::LuaState};
#[cfg_attr(not(size_optimized), inline)]
pub fn compile<'a>(
    state: &mut LuaState<'a>,
    source: &str,
    name: &str,
) -> Result<Proto<'a>, String> {
    let mut ls = lexer::LexState::new(state, source, name);
    compile::compile_chunk(&mut ls)
}
