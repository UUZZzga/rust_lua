return {
    name = "GC压力分配",
    full = 2000000, quick = 200000,
    fn = function(n)
        local s = 0
        for i = 1, n do
            local t = { a = i, b = "str" .. i % 100 }
            s = s + t.a
        end
        return s
    end,
}
