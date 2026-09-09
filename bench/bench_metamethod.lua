return {
    name = "元方法调用",
    full = 1000000, quick = 100000,
    fn = function(n)
        local mt = {
            __index = function(_, k) return k end,
            __add = function() return 1 end,
        }
        local t = setmetatable({}, mt)
        local u = setmetatable({}, mt)
        local s = 0
        for i = 1, n do s = s + t[i % 100] + (u + u) end
        return s
    end,
}
