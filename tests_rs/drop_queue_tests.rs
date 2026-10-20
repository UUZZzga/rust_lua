//! 延迟释放队列的不变量测试 —— `write_stack` 把旧槽值的 `Rc` 释放出线到
//! `VmExecutor::push_drop`（push-only），入队 `LuaState.drop_queue`，批量排空时才算
//! 引用计数减一。排空点有三类：
//!
//! * 帧边界：`CALL` / `TAILCALL` / `RETURN` / `RETURN0` / `RETURN1` 各一次
//!   `LuaState::drain_drop_queue_if_full`（放在各 opcode 的 match 之前，分支漏不掉）；
//! * GC 入口：`collect_gc` 里**无条件**排空（不做长度判定）；
//! * 回边兜底：`backedge_tick` 复用已有的 64 次回边分支判满排空 —— 帧边界排空对
//!   「一段不调用任何东西的循环」不生效，靠这里把**峰值**压住。
//!
//! 本文件钉住两条性质（都从 Rust 侧直接观测队列）：
//!
//! 1. **峰值有界且与迭代次数无关**：无调用循环里容量停在常数级（上界 + 一个检查窗口）。
//!    余量按 4096 写，去掉回边兜底时实测随迭代线性增长（100000 次 → 131072）。
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

/// 无调用循环里的峰值**与迭代次数无关**（回边兜底），而不是「≤ 预留容量」。
///
/// 断言的写法有讲究：
/// * 峰值只能靠容量观测（残留长度在 chunk 返回时已被帧边界排空归零），而容量会因 **一个
///   检查窗口内的超额入队** 合法地翻倍一次 —— 回边兜底每 64 次回边才判一次，窗口里最多再
///   入队 64 项，故峰值 ≈ `DROP_QUEUE_LIMIT + 64`（实测容量 512）。因此不能断言「容量恒等于
///   预留值」，那会随 tick 相位翻转（同一份代码在 100000 次迭代下先过、后挂）。
/// * 也不能断言「少量覆盖后队列非空」：入队与观测之间任意一次 GC 都会排空队列（CI #238 的
///   Linux release 就是这么挂的，而 Windows release 通过）。
/// * 真正的不变量是**有界性**：容量停在常数级且与迭代次数无关。去掉回边兜底时它随迭代线性
///   增长（100000 次 → 131072，1000000 次 → 更大），与 4096 的余量相差两个数量级。
#[test]
fn drop_queue_peak_is_bounded() {
    let mut st = new_state();

    run(
        &mut st,
        "local a = {} local x\n\
         for i = 1, 1000000 do x = a end",
    );
    assert!(
        st.drop_queue.len() <= 4096,
        "队列长度 {} 发散（回边兜底排空失效？）",
        st.drop_queue.len()
    );
    assert!(
        st.drop_queue.capacity() <= 4096,
        "队列容量涨到 {} —— 无调用循环里死值随迭代线性累积，回边兜底排空失效（上界 {}）",
        st.drop_queue.capacity(),
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

/// 帧边界必须排空**满**队列 —— 直线段（无回边、无调用）把队列填满后，chunk 末尾的
/// `RETURN` / `RETURN0` / `RETURN1` 是唯一排空点。
///
/// 去掉五处帧边界调用后本用例实测残留 299 项（右值断言失败），因此它守的是「帧边界排空
/// 集合」整体。`RETURN0` / `RETURN1` 之前整段漏排空：对**残留**无影响（外层 `CALL` 边界
/// 会补上），但满队列多活一段；现在五个 opcode 一律在 match **之前**调用一次
/// `drain_drop_queue_if_full`，新增分支不可能再漏。
#[test]
fn frame_boundary_drains_a_full_queue() {
    let mut st = new_state();

    // 300 条直线赋值（> 上界 256）：无回边、无调用，填满队列。
    let mut src = String::from("local a = {} local x\n");
    for _ in 0..300 {
        src.push_str("x = a\n");
    }
    run(&mut st, &src);
    assert_eq!(
        st.drop_queue.len(),
        0,
        "chunk 结尾的帧边界没有排空满队列（RETURN0/RETURN1 漏排空？）"
    );
}
