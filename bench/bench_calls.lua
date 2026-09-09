return {
    name = "函数调用vararg",
    full = 2000000, quick = 200000,
    fn = function(n)
        local function add(a, b) return a + b end
        local function pass(...)
            return add(select(1, ...), select(2, ...))
        end
        local s = 0
        for i = 1, n do s = s + pass(i, i) end
        return s
    end,
}
