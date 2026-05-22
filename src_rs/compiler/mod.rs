pub mod lexer;
pub mod compile;
pub mod bytecode_dump;
#[cfg(test)]
mod cmp_tests;

use crate::objects::Proto;

pub fn compile(source: &str, name: &str) -> Result<Proto, String> {
    let mut ls = lexer::LexState::new(source, name);
    compile::compile_chunk(&mut ls)
}