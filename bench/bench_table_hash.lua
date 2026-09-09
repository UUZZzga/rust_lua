return {
    name = "哈希表查找",
    full = 2000000, quick = 200000,
    fn = function(n)
        local t = {}
        for i = 1, n do t["key_" .. i % 4096] = i end
        local s = 0
        for i = 1, n do s = s + (t["key_" .. i % 4096] or 0) end
        return s
    end,
}
