return {
    name = "加载编译",
    full = 2000, quick = 200,
    fn = function(n)
        local src = "local a = 1\n"
        for i = 1, 200 do src = src .. "a = a + " .. i .. "\n" end
        src = src .. "return a"
        local s = 0
        for _ = 1, n do
            local f = assert(load(src))
            s = s + f()
        end
        return s
    end,
}
