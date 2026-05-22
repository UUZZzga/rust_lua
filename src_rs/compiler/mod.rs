pub mod lexer;
pub mod compile;

use crate::objects::Proto;

pub fn compile(source: &str, name: &str) -> Result<Proto, String> {
    let mut ls = lexer::LexState::new(source, name);
    compile::compile_chunk(&mut ls)
}