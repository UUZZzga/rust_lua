fn main() {
    println!("TValue: {}", std::mem::size_of::<lua_rs::objects::TValue>());
    println!("GCObjectHeader: {}", std::mem::size_of::<lua_rs::gc::GCObjectHeader>());
    println!("Table: {}", std::mem::size_of::<lua_rs::objects::Table>());
    println!("LuaString: {}", std::mem::size_of::<lua_rs::strings::LuaString>());
    println!("LClosure: {}", std::mem::size_of::<lua_rs::objects::LClosure>());
    println!("CClosure: {}", std::mem::size_of::<lua_rs::objects::CClosure>());
    println!("Box<LClosure>: {}", std::mem::size_of::<Box<lua_rs::objects::LClosure>>());
    println!("UpVal: {}", std::mem::size_of::<lua_rs::objects::UpVal>());
    println!("UpValRef: {}", std::mem::size_of::<lua_rs::objects::UpValRef>());
    println!("Proto: {}", std::mem::size_of::<lua_rs::objects::Proto>());
    println!("Instruction: {}", std::mem::size_of::<lua_rs::objects::Instruction>());
    println!("CallFrame: {}", std::mem::size_of::<lua_rs::objects::CallFrame>());
    println!("LuaThread: {}", std::mem::size_of::<lua_rs::objects::LuaThread>());
    println!("Vec<TValue>: {}", std::mem::size_of::<Vec<lua_rs::objects::TValue>>());
    println!("Rc<RefCell<TableData>>: {}", std::mem::size_of::<std::rc::Rc<std::cell::RefCell<lua_rs::objects::TableData>>>());
    println!("Option<Box<hashbrown::HashMap<lua_rs::objects::TValue, usize>>>: {}", 
        std::mem::size_of::<Option<Box<hashbrown::HashMap<lua_rs::objects::TValue, usize>>>>());
    println!("ThreadContext: {}", std::mem::size_of::<lua_rs::objects::ThreadContext>());
}
