//! Lua 表实现 (基于 `crate::objects::Table`)
//!
//! `Table` 的定义在 [crate::objects] 中；此处包含所有方法实现及测试。
//!
//! ## 设计原则
//! - 数组部分：`Vec<TValue>`，1-based，空槽存储 `Nil(Empty)`
//! - 哈希部分：`hashbrown::HashMap<TValue, TValue>`
//! - `LuaTable` 封装为未来元方法支持预留接口

use crate::objects::{NilKind, TValue};
use crate::gc::GCObjectHeader;

pub use crate::objects::Table;

// ============================================================================
// Table 方法实现
// ============================================================================

impl Table {
    pub fn new() -> Self {
        Table {
            gc_header: GCObjectHeader::new(),
            array: Vec::new(),
            hash: hashbrown::HashMap::new(),
            metatable: None,
            len_hint: 0,
        }
    }

    pub fn with_capacity(narray: usize, nhash: usize) -> Self {
        Table {
            gc_header: GCObjectHeader::new(),
            array: (0..narray).map(|_| TValue::Nil(NilKind::Empty)).collect(),
            hash: hashbrown::HashMap::with_capacity(nhash),
            metatable: None,
            len_hint: narray / 2,
        }
    }

    pub fn array_size(&self) -> usize {
        self.array.len()
    }

    pub fn hash_size(&self) -> usize {
        self.hash.len()
    }

    // ========================================================================
    // get / get_int
    // ========================================================================

    pub fn get(&self, key: &TValue) -> Option<&TValue> {
        match key {
            TValue::Integer(i) if *i > 0 => {
                let idx = (*i - 1) as usize;
                if idx < self.array.len() {
                    let v = &self.array[idx];
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some(v);
                    }
                }
                self.hash.get(key)
            }
            TValue::Float(f) => {
                let i = *f as i64;
                if (i as f64).to_bits() == f.to_bits() && i > 0 && *f != -0.0 {
                    let idx = (i - 1) as usize;
                    if idx < self.array.len() {
                        let v = &self.array[idx];
                        if !matches!(v, TValue::Nil(NilKind::Empty)) {
                            return Some(v);
                        }
                    }
                }
                self.hash.get(key)
            }
            _ => self.hash.get(key),
        }
    }

    pub fn get_int(&self, key: i64) -> Option<&TValue> {
        if key > 0 {
            let idx = (key - 1) as usize;
            if idx < self.array.len() {
                let v = &self.array[idx];
                if !matches!(v, TValue::Nil(NilKind::Empty)) {
                    return Some(v);
                }
            }
        }
        self.hash.get(&TValue::Integer(key))
    }

    // ========================================================================
    // set / set_int
    // ========================================================================

    pub fn set(&mut self, key: TValue, value: TValue) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        match &key {
            TValue::Integer(i) if *i > 0 => {
                let idx = (*i - 1) as usize;
                if idx < self.array.len() {
                    if is_nil {
                        self.array[idx] = TValue::Nil(NilKind::Empty);
                    } else {
                        self.array[idx] = value;
                    }
                    return;
                }
            }
            TValue::Float(f) => {
                let i = *f as i64;
                if (i as f64).to_bits() == f.to_bits() && i > 0 && *f != -0.0 {
                    let idx = (i - 1) as usize;
                    if idx < self.array.len() {
                        if is_nil {
                            self.array[idx] = TValue::Nil(NilKind::Empty);
                        } else {
                            self.array[idx] = value;
                        }
                        return;
                    }
                }
            }
            _ => {}
        }
        if is_nil {
            self.hash.remove(&key);
        } else {
            self.hash.insert(key, value);
        }
    }

    pub fn set_int(&mut self, key: i64, value: TValue) {
        let is_nil = matches!(&value, TValue::Nil(NilKind::Strict));
        if key > 0 {
            let idx = (key - 1) as usize;
            if idx < self.array.len() {
                if is_nil {
                    self.array[idx] = TValue::Nil(NilKind::Empty);
                } else {
                    self.array[idx] = value;
                }
                return;
            }
        }
        let k = TValue::Integer(key);
        if is_nil {
            self.hash.remove(&k);
        } else {
            self.hash.insert(k, value);
        }
    }

    // ========================================================================
    // len: # 操作符
    // ========================================================================

    pub fn len(&self) -> i64 {
        self.compute_len()
    }

    fn compute_len(&self) -> i64 {
        let asize = self.array.len();
        if asize == 0 {
            return self.hash_boundary();
        }

        let hint = if self.len_hint > 0 && self.len_hint <= asize {
            self.len_hint
        } else {
            1
        };

        let present_at = |i: usize| -> bool {
            i > 0 && i <= asize && !matches!(&self.array[i - 1], TValue::Nil(NilKind::Empty))
        };

        if present_at(hint) {
            let mut limit = hint;
            for _ in 0..4 {
                if limit >= asize {
                    break;
                }
                limit += 1;
                if !present_at(limit) {
                    return limit as i64 - 1;
                }
            }
            if !present_at(asize) {
                return Self::bin_search_array(&self.array, limit, asize) as i64;
            }
        } else {
            let mut limit = hint;
            for _ in 0..4 {
                if limit <= 1 {
                    break;
                }
                limit -= 1;
                if present_at(limit) {
                    return limit as i64;
                }
            }
            return Self::bin_search_array(&self.array, 0, limit) as i64;
        }

        if self.hash.is_empty() {
            return asize as i64;
        }
        self.hash_boundary()
    }

    fn bin_search_array(array: &[TValue], lo: usize, hi: usize) -> usize {
        let mut i = lo;
        let mut j = hi;
        while j - i > 1 {
            let m = (i + j) / 2;
            if matches!(&array[m - 1], TValue::Nil(NilKind::Empty)) {
                j = m;
            } else {
                i = m;
            }
        }
        i
    }

    fn hash_boundary(&self) -> i64 {
        let asize = self.array.len() as i64;
        if self.hash.contains_key(&TValue::Integer(asize + 1)) {
            let mut i = asize as u64 + 1;
            let mut j = i * 2;
            loop {
                if !self.hash.contains_key(&TValue::Integer(j as i64)) {
                    break;
                }
                i = j;
                if j > (i64::MAX as u64) / 2 {
                    if !self.hash.contains_key(&TValue::Integer(i64::MAX)) {
                        j = i64::MAX as u64;
                        break;
                    }
                    return i64::MAX;
                }
                j *= 2;
            }
            while j - i > 1 {
                let m = (i + j) / 2;
                if self.hash.contains_key(&TValue::Integer(m as i64)) {
                    i = m;
                } else {
                    j = m;
                }
            }
            i as i64
        } else {
            asize
        }
    }

    // ========================================================================
    // next: 表遍历
    // ========================================================================

    pub fn next(&self, prev_key: Option<&TValue>) -> Option<(TValue, TValue)> {
        match prev_key {
            None => {
                for (i, v) in self.array.iter().enumerate() {
                    if !matches!(v, TValue::Nil(NilKind::Empty)) {
                        return Some((TValue::Integer(i as i64 + 1), v.clone()));
                    }
                }
                for (k, v) in self.hash.iter() {
                    return Some((k.clone(), v.clone()));
                }
                None
            }
            Some(prev) => match prev {
                TValue::Integer(i) if *i > 0 => {
                    let idx = *i as usize;
                    for j in idx..self.array.len() {
                        let v = &self.array[j];
                        if !matches!(v, TValue::Nil(NilKind::Empty)) {
                            return Some((TValue::Integer(j as i64 + 1), v.clone()));
                        }
                    }
                    for (k, v) in self.hash.iter() {
                        return Some((k.clone(), v.clone()));
                    }
                    None
                }
                _ => {
                    let mut found = false;
                    for (k, v) in self.hash.iter() {
                        if found {
                            return Some((k.clone(), v.clone()));
                        }
                        if k == prev {
                            found = true;
                        }
                    }
                    None
                }
            },
        }
    }

    // ========================================================================
    // rehash
    // ========================================================================

    pub fn rehash(&mut self, nasize: usize, nhsize: usize) {
        let mut all_entries: Vec<(TValue, TValue)> = Vec::new();

        for (i, v) in self.array.iter().enumerate() {
            if !matches!(v, TValue::Nil(NilKind::Empty)) {
                all_entries.push((TValue::Integer(i as i64 + 1), v.clone()));
            }
        }
        for (k, v) in self.hash.drain() {
            all_entries.push((k, v));
        }

        let old_asize = self.array.len();
        if nasize > old_asize {
            self.array.resize(nasize, TValue::Nil(NilKind::Empty));
        } else {
            self.array.truncate(nasize);
        }
        self.hash.clear();
        self.hash.reserve(nhsize);
        self.len_hint = nasize / 2;

        for (k, v) in all_entries {
            match &k {
                TValue::Integer(i) if *i > 0 => {
                    let idx = (*i - 1) as usize;
                    if idx < nasize {
                        self.array[idx] = v;
                    } else {
                        self.hash.insert(k, v);
                    }
                }
                _ => {
                    self.hash.insert(k, v);
                }
            }
        }
    }

    pub fn resize_array(&mut self, nasize: usize) {
        let nhsize = self.hash.len();
        self.rehash(nasize, nhsize);
    }

    pub fn mem_size(&self) -> usize {
        std::mem::size_of::<Table>()
            + self.array.len() * std::mem::size_of::<TValue>()
            + self.hash.capacity()
                * (std::mem::size_of::<TValue>() * 2 + std::mem::size_of::<u8>())
    }
}

// ============================================================================
// LuaTable: 元方法感知封装
// ============================================================================

pub struct LuaTable {
    table: Table,
}

impl LuaTable {
    pub fn new() -> Self {
        LuaTable {
            table: Table::new(),
        }
    }

    pub fn with_capacity(narray: usize, nhash: usize) -> Self {
        LuaTable {
            table: Table::with_capacity(narray, nhash),
        }
    }

    pub fn inner(&self) -> &Table {
        &self.table
    }

    pub fn inner_mut(&mut self) -> &mut Table {
        &mut self.table
    }

    pub fn get(&self, key: &TValue) -> Option<&TValue> {
        self.table.get(key)
    }

    pub fn get_int(&self, key: i64) -> Option<&TValue> {
        self.table.get_int(key)
    }

    pub fn set(&mut self, key: TValue, value: TValue) {
        self.table.set(key, value);
    }

    pub fn set_int(&mut self, key: i64, value: TValue) {
        self.table.set_int(key, value);
    }

    pub fn len(&self) -> i64 {
        self.table.len()
    }

    pub fn next(&self, prev_key: Option<&TValue>) -> Option<(TValue, TValue)> {
        self.table.next(prev_key)
    }

    pub fn rehash(&mut self, nasize: usize, nhsize: usize) {
        self.table.rehash(nasize, nhsize);
    }

    pub fn resize_array(&mut self, nasize: usize) {
        self.table.resize_array(nasize);
    }
}

impl Default for LuaTable {
    fn default() -> Self {
        Self::new()
    }
}

pub fn new_table() -> Table {
    Table::new()
}

pub fn new_table_with_capacity(narray: usize, nhsize: usize) -> Table {
    Table::with_capacity(narray, nhsize)
}

// ============================================================================
// 测试 (TDD)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::{NilKind, TValue};
    use crate::strings::{LuaString, ShortString};

    // ------------------------------------------------------------------------
    // 构造 & 容量
    // ------------------------------------------------------------------------

    #[test]
    fn test_table_new() {
        let t = Table::new();
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 0);
        assert!(t.metatable.is_none());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_table_with_capacity() {
        let t = Table::with_capacity(10, 8);
        assert_eq!(t.array_size(), 10);
        assert!(t.hash.capacity() >= 8);
        for v in &t.array {
            assert!(matches!(v, TValue::Nil(NilKind::Empty)));
        }
    }

    #[test]
    fn test_table_default() {
        let t = Table::default();
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 0);
    }

    #[test]
    fn test_lua_table_new() {
        let lt = LuaTable::new();
        assert_eq!(lt.len(), 0);
    }

    // ------------------------------------------------------------------------
    // get / get_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_get_int_array_present() {
        let mut t = Table::with_capacity(5, 0);
        t.array[0] = TValue::Integer(10);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
    }

    #[test]
    fn test_get_int_array_empty_slot() {
        let t = Table::with_capacity(5, 0);
        assert_eq!(t.get_int(1), None);
    }

    #[test]
    fn test_get_int_out_of_array() {
        let t = Table::new();
        assert_eq!(t.get_int(100), None);
    }

    #[test]
    fn test_get_int_hash() {
        let mut t = Table::new();
        t.hash.insert(TValue::Integer(100), TValue::Integer(42));
        assert_eq!(t.get_int(100), Some(&TValue::Integer(42)));
    }

    #[test]
    fn test_get_int_zero_or_negative() {
        let mut t = Table::new();
        t.hash.insert(TValue::Integer(0), TValue::Integer(99));
        assert_eq!(t.get_int(0), Some(&TValue::Integer(99)));
        assert_eq!(t.get_int(-1), None);
    }

    #[test]
    fn test_get_string_key() {
        let mut t = Table::new();
        let key = LuaString::Short(std::sync::Arc::new(ShortString {
            hash: 0,
            contents: "name".into(),
        }));
        t.hash.insert(TValue::Str(key.clone()), TValue::Integer(42));
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(&TValue::Integer(42)));
    }

    #[test]
    fn test_get_float_integral() {
        let mut t = Table::with_capacity(5, 0);
        t.array[0] = TValue::Integer(10);
        assert_eq!(t.get(&TValue::Float(1.0)), Some(&TValue::Integer(10)));
    }

    #[test]
    fn test_get_float_non_integral() {
        let mut t = Table::new();
        t.hash.insert(TValue::Float(3.14), TValue::Integer(42));
        assert_eq!(t.get(&TValue::Float(3.14)), Some(&TValue::Integer(42)));
    }

    #[test]
    fn test_get_nil_key() {
        let t = Table::new();
        assert_eq!(t.get(&TValue::Nil(NilKind::Strict)), None);
    }

    #[test]
    fn test_get_with_duplicate_keys_float_int() {
        let mut t = Table::new();
        t.hash.insert(TValue::Integer(42), TValue::Integer(100));
        // Float(42.0) 和 Integer(42) 在 Lua 中等价
        assert_eq!(t.get(&TValue::Float(42.0)), Some(&TValue::Integer(100)));
    }

    // ------------------------------------------------------------------------
    // set / set_int
    // ------------------------------------------------------------------------

    #[test]
    fn test_set_int_array() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        assert_eq!(t.array[0], TValue::Integer(10));
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
    }

    #[test]
    fn test_set_int_hash() {
        let mut t = Table::new();
        t.set_int(100, TValue::Integer(42));
        assert!(t.hash.contains_key(&TValue::Integer(100)));
        assert_eq!(t.get_int(100), Some(&TValue::Integer(42)));
    }

    #[test]
    fn test_set_overwrite() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(1, TValue::Integer(99));
        assert_eq!(t.get_int(1), Some(&TValue::Integer(99)));
    }

    #[test]
    fn test_set_nil_removes() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        assert!(t.get_int(1).is_some());
        t.set_int(1, TValue::Nil(NilKind::Strict));
        assert_eq!(t.get_int(1), None);
        assert!(matches!(&t.array[0], TValue::Nil(NilKind::Empty)));
    }

    #[test]
    fn test_set_nil_removes_from_hash() {
        let mut t = Table::new();
        t.set_int(100, TValue::Integer(42));
        assert_eq!(t.hash_size(), 1);
        t.set_int(100, TValue::Nil(NilKind::Strict));
        assert_eq!(t.hash_size(), 0);
    }

    #[test]
    fn test_set_string_key() {
        let mut t = Table::new();
        let key = LuaString::Short(std::sync::Arc::new(ShortString {
            hash: 0,
            contents: "key".into(),
        }));
        t.set(TValue::Str(key.clone()), TValue::Integer(7));
        assert_eq!(t.hash_size(), 1);
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(&TValue::Integer(7)));
    }

    #[test]
    fn test_set_bool_key() {
        let mut t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        t.set(TValue::Boolean(false), TValue::Integer(0));
        assert_eq!(t.hash_size(), 2);
        assert_eq!(t.get(&TValue::Boolean(true)), Some(&TValue::Integer(1)));
        assert_eq!(t.get(&TValue::Boolean(false)), Some(&TValue::Integer(0)));
    }

    #[test]
    fn test_set_chained_keys() {
        let mut t = Table::new();
        for i in 1..=100 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        // 无数组，但 hash 中有 1..100 连续整数键，len() 可以找到边界
        assert_eq!(t.len(), 100);
        assert_eq!(t.hash_size(), 100);
        assert_eq!(t.get_int(50), Some(&TValue::Integer(500)));
    }

    // ------------------------------------------------------------------------
    // len (#t)
    // ------------------------------------------------------------------------

    #[test]
    fn test_len_dense_array() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn test_len_hole() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn test_len_empty_table() {
        let t = Table::new();
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_len_full_array() {
        let mut t = Table::with_capacity(4, 0);
        for i in 1..=4 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn test_len_hash_only() {
        let mut t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_len_array_then_hash_continuation() {
        let mut t = Table::with_capacity(3, 10);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        t.hash.insert(TValue::Integer(4), TValue::Integer(40));
        t.hash.insert(TValue::Integer(5), TValue::Integer(50));
        assert_eq!(t.len(), 5);
    }

    // ------------------------------------------------------------------------
    // next
    // ------------------------------------------------------------------------

    #[test]
    fn test_next_from_nil() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        let first = t.next(None);
        assert!(first.is_some());
        let (k, v) = first.unwrap();
        assert_eq!(k, TValue::Integer(1));
        assert_eq!(v, TValue::Integer(10));
    }

    #[test]
    fn test_next_array_full_traversal() {
        let mut t = Table::with_capacity(3, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));

        let r1 = t.next(None).unwrap();
        assert_eq!(r1, (TValue::Integer(1), TValue::Integer(10)));

        let r2 = t.next(Some(&r1.0)).unwrap();
        assert_eq!(r2, (TValue::Integer(2), TValue::Integer(20)));

        let r3 = t.next(Some(&r2.0)).unwrap();
        assert_eq!(r3, (TValue::Integer(3), TValue::Integer(30)));

        assert_eq!(t.next(Some(&r3.0)), None);
    }

    #[test]
    fn test_next_with_hole() {
        let mut t = Table::with_capacity(4, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));

        let r1 = t.next(None).unwrap();
        assert_eq!(r1.0, TValue::Integer(1));

        let r2 = t.next(Some(&r1.0)).unwrap();
        assert_eq!(r2.0, TValue::Integer(3));

        assert_eq!(t.next(Some(&r2.0)), None);
    }

    #[test]
    fn test_next_hash_part() {
        let mut t = Table::new();
        let key_a = TValue::Boolean(true);
        t.set(key_a.clone(), TValue::Integer(1));

        let r = t.next(None).unwrap();
        assert_eq!(r, (key_a.clone(), TValue::Integer(1)));
        assert_eq!(t.next(Some(&r.0)), None);
    }

    #[test]
    fn test_next_empty_table() {
        let t = Table::new();
        assert_eq!(t.next(None), None);
    }

    #[test]
    fn test_next_array_then_hash() {
        let mut t = Table::with_capacity(2, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set(TValue::Boolean(true), TValue::Integer(99));

        let mut keys_seen = Vec::new();
        let mut k_opt: Option<TValue> = None;
        while let Some((k, _v)) = t.next(k_opt.as_ref()) {
            keys_seen.push(k.clone());
            k_opt = Some(k);
        }
        assert_eq!(keys_seen.len(), 3);
        assert_eq!(keys_seen[0], TValue::Integer(1));
        assert_eq!(keys_seen[1], TValue::Integer(2));
    }

    #[test]
    fn test_next_multiple_hash_keys() {
        let mut t = Table::new();
        t.set(TValue::Boolean(true), TValue::Integer(1));
        t.set(TValue::Boolean(false), TValue::Integer(2));
        t.set(TValue::Integer(0), TValue::Integer(3));

        let mut keys_seen = Vec::new();
        let mut k_opt: Option<TValue> = None;
        while let Some((k, _)) = t.next(k_opt.as_ref()) {
            keys_seen.push(k);
            k_opt = Some(keys_seen.last().unwrap().clone());
        }
        assert_eq!(keys_seen.len(), 3);
    }

    #[test]
    fn test_next_after_nil_set() {
        let mut t = Table::with_capacity(5, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.set_int(3, TValue::Integer(30));
        t.set_int(2, TValue::Nil(NilKind::Strict)); // 删除键 2

        let all: Vec<_> = {
            let mut v = Vec::new();
            let mut k_opt: Option<TValue> = None;
            while let Some((k, _)) = t.next(k_opt.as_ref()) {
                v.push(k);
                k_opt = Some(v.last().unwrap().clone());
            }
            v
        };
        assert_eq!(all.len(), 2);
        assert!(all.contains(&TValue::Integer(1)));
        assert!(all.contains(&TValue::Integer(3)));
    }

    // ------------------------------------------------------------------------
    // rehash
    // ------------------------------------------------------------------------

    #[test]
    fn test_rehash_expand_array() {
        let mut t = Table::new();
        t.set_int(1, TValue::Integer(10));
        t.set_int(3, TValue::Integer(30));
        assert_eq!(t.array_size(), 0);

        t.rehash(3, 0);
        assert_eq!(t.array_size(), 3);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
        assert_eq!(t.get_int(3), Some(&TValue::Integer(30)));
        assert!(matches!(&t.array[1], TValue::Nil(NilKind::Empty)));
    }

    #[test]
    fn test_rehash_shrink_array() {
        let mut t = Table::with_capacity(5, 0);
        for i in 1..=5 {
            t.set_int(i, TValue::Integer(i * 10));
        }
        assert_eq!(t.array_size(), 5);

        t.rehash(2, 0);
        assert_eq!(t.array_size(), 2);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
        assert_eq!(t.get_int(2), Some(&TValue::Integer(20)));
        assert_eq!(t.hash_size(), 3);
        assert_eq!(t.get_int(3), Some(&TValue::Integer(30)));
        assert_eq!(t.get_int(5), Some(&TValue::Integer(50)));
    }

    #[test]
    fn test_resize_array() {
        let mut t = Table::with_capacity(2, 0);
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));
        t.resize_array(4);
        assert_eq!(t.array_size(), 4);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
        assert_eq!(t.get_int(2), Some(&TValue::Integer(20)));
    }

    #[test]
    fn test_rehash_preserves_string_keys() {
        let mut t = Table::new();
        let key = LuaString::Short(std::sync::Arc::new(ShortString {
            hash: 0,
            contents: "mykey".into(),
        }));
        t.set(TValue::Str(key.clone()), TValue::Integer(77));
        t.set_int(1, TValue::Integer(10));

        t.rehash(5, 10);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
        let lookup = TValue::Str(key);
        assert_eq!(t.get(&lookup), Some(&TValue::Integer(77)));
    }

    #[test]
    fn test_rehash_twice() {
        let mut t = Table::new();
        t.set_int(1, TValue::Integer(10));
        t.set_int(2, TValue::Integer(20));

        t.rehash(5, 0);
        assert_eq!(t.array_size(), 5);
        assert_eq!(t.len(), 2);

        t.rehash(0, 10);
        assert_eq!(t.array_size(), 0);
        assert_eq!(t.hash_size(), 2);
        assert_eq!(t.get_int(1), Some(&TValue::Integer(10)));
    }

    #[test]
    fn test_mem_size() {
        let t = Table::with_capacity(10, 16);
        let size = t.mem_size();
        assert!(size > 0);
    }

    // ------------------------------------------------------------------------
    // LuaTable 封装
    // ------------------------------------------------------------------------

    #[test]
    fn test_lua_table_get_set() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(42));
        assert_eq!(lt.get_int(1), Some(&TValue::Integer(42)));
    }

    #[test]
    fn test_lua_table_len() {
        let mut lt = LuaTable::with_capacity(3, 0);
        lt.set_int(1, TValue::Integer(10));
        lt.set_int(2, TValue::Integer(20));
        lt.set_int(3, TValue::Integer(30));
        assert_eq!(lt.len(), 3);
    }

    #[test]
    fn test_lua_table_inner_access() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(99));
        assert_eq!(lt.inner().get_int(1), Some(&TValue::Integer(99)));
        lt.inner_mut().set_int(2, TValue::Integer(88));
        assert_eq!(lt.get_int(2), Some(&TValue::Integer(88)));
    }

    #[test]
    fn test_lua_table_next() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(10));
        let r = lt.next(None).unwrap();
        assert_eq!(r, (TValue::Integer(1), TValue::Integer(10)));
    }

    #[test]
    fn test_lua_table_rehash() {
        let mut lt = LuaTable::new();
        lt.set_int(1, TValue::Integer(10));
        lt.set_int(3, TValue::Integer(30));
        lt.rehash(3, 0);
        assert_eq!(lt.len(), 1);
    }

    #[test]
    fn test_new_table_functions() {
        let t = new_table();
        assert_eq!(t.array_size(), 0);
        let t2 = new_table_with_capacity(5, 8);
        assert_eq!(t2.array_size(), 5);
    }
}