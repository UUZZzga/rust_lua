return {
    name = "协程切换",
    full = 2000000, quick = 200000,
    fn = function(n)
        local co = coroutine.create(function()
            while true do coroutine.yield(1) end
        end)
        local s = 0
        for i = 1, n do
            local _, v = coroutine.resume(co)
            s = s + v
        end
        return s
    end,
}
