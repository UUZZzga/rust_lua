use std::fmt::{self, Write}; // 这里 import 是为了 impl 内部能用 write_str 等方法

pub struct SliceWriter<'a> {
    buf: &'a mut [u8],
    pub pos: usize, // 已写入的字节数（不含结尾的 \0）
}

impl<'a> SliceWriter<'a> {
    /// buf 必须非空；实际可写 buf.len()-1 字节
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(!buf.is_empty(), "buffer must be at least 1 byte for NUL");
        buf[0] = 0;
        Self { buf, pos: 0 }
    }

    /// 返回不含 NUL 的已写入内容
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.pos]
    }

    /// 返回 C 字符串指针（保证末尾是 \0）
    pub fn as_c_str(&self) -> *const std::os::raw::c_char {
        self.buf.as_ptr() as *const _
    }

    /// 返回含 NUL 的完整切片
    pub fn as_bytes_with_nul(&self) -> &[u8] {
        &self.buf[..=self.pos]
    }
}

impl fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let end = self.pos + bytes.len();
        if end >= self.buf.len() {
            // 放不下：要求 end + 1(\0) <= buf.len()
            return Err(fmt::Error);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        self.buf[self.pos] = 0; // 每次写完补 \0
        Ok(())
    }
}
