return {
    name = "闭包创建",
    full = 2000000, quick = 200000,
    fn = function(n)
        local acc = 0
        for i = 1, n do
            local j = i
            local f = function() return j end
            acc = acc + f()
        end
        return acc
    end,
}
