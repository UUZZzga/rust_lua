return {
    name = "字符串模式匹配",
    full = 50000, quick = 20000,
    fn = function(n)
        local subject = string.rep(("abc def %d %s xyz\n"):format(42, "k"), 256)
        local c = 0
        for i = 1, n do
            local s = subject:gsub("%d+", "N")
            c = c + #s
            for w in subject:gmatch("%a+") do c = c + 1 end
        end
        return c
    end,
}
