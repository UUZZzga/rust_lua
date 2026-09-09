return {
    name = "整数算术",
    full = 20000000, quick = 2000000,
    fn = function(n)
        local s = 0
        for i = 1, n do s = (s + i * 3) % 1000003 end
        return s
    end,
}
