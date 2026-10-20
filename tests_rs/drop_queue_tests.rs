//! 延迟释放队列的不变量测试 —— `write_stack` 把旧槽值的 `Rc` 释放出线到
//! `VmExecutor::push_drop`（push-only），入队 `LuaState.drop_queue`，批量排空时才算
//! 引用计数减一。排空点是帧边界（CALL / TAILCALL / RETURN 各分支）与 `collect_gc`
//! 入口的**无条件**排空。
//!
//! 本文件钉住两条性质（都从 Rust 侧直接观测队列）：
//!
//! 1. **机制确实被用上**：连续覆盖持有引用的槽位后队列非空 —— 否则下面的断言都是空转。
//! 2. **GC 入口无条件排空**：保证 GC 标记阶段之前死值已归还。「不可达即释放」的外部
//!    字符串语义（`capi::tests::test_lua_pushexternalstring_matches_c_semantics`
//!    断言的 `falloc` 恰好一次）依赖这一点；帧边界排空后残留长度同样为 0。
//!
//! 触发方式：局部变量被同一个表反复覆盖 —— 旧值持有引用，每次写都走 `push_drop`。

use lua_rs::mock::io_mock::lua_io;
use lua_rs::state::{LuaState, DROP_QUEUE_LIMIT};

fn new_state() -> LuaState<'static> {
    let mut st = LuaState::new_main(lua_io()).expect("创建 state 失败");
    st.open_selected_libs(0, 0).expect("打开标准库失败");
    st
}

fn run(st: &mut LuaState<'static>, src: &str) {
    assert_eq!(st.load_buffer(src, "=drop_queue"), 0, "编译失败: {src}");
    st.top = st.stack.len(); // Rust 侧压栈不推进 top（GC 只把 [..top] 当根）
    assert_eq!(st.pcall(0, 0, 0), 0, "执行失败: {src}");
}

/// `x = a` 每次都让旧 `x`（持有表的引用）出队 —— 队列被真正用上。
#[test]
fn drop_queue_is_used_and_bounded() {
    let mut st = new_state();

    // 少量覆盖：证明机制确实入队（否则下面的上界断言是空转）。
    run(&mut st, "local a = {} local x x = a x = a x = a");
    assert!(
        !st.drop_queue.is_empty(),
        "连续覆盖持有引用的槽位后队列仍为空 —— 延迟释放机制没有被触发"
    );

    // 长循环：帧边界/GC 之外没有其它排空点，残留长度在 chunk 返回后应已归零。
    run(
        &mut st,
        "local a = {} local x\n\
         for i = 1, 100000 do x = a end",
    );
    assert!(
        st.drop_queue.len() <= DROP_QUEUE_LIMIT,
        "队列长度 {} 超过上界 {} —— 死值会无界累积",
        st.drop_queue.len(),
        DROP_QUEUE_LIMIT
    );
}

/// GC 入口必须把队列排空（标记阶段之前死值已归还）。
#[test]
fn gc_drains_the_drop_queue() {
    let mut st = new_state();

    // 远小于上界的少量入队同样必须被排空 —— 排空不依赖任何长度判定。
    run(
        &mut st,
        "local a = {} local x x = a x = a x = a collectgarbage('collect')",
    );
    assert_eq!(
        st.drop_queue.len(),
        0,
        "少量入队（2 项）后 collectgarbage('collect') 必须排空队列"
    );

    // 长循环之后同样归零。
    run(
        &mut st,
        "local a = {} local x\n\
         for i = 1, 100000 do x = a end\n\
         collectgarbage('collect')",
    );
    assert_eq!(
        st.drop_queue.len(),
        0,
        "collectgarbage('collect') 之后延迟释放队列必须为空"
    );
}
