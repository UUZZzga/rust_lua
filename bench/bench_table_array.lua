return {
    name = "表插入遍历",
    full = 2000000, quick = 200000,
    fn = function(n)
        local t = {}
        for i = 1, n do t[i] = i end
        local s = 0
        for i = 1, n do s = s + t[i] end
        return #table.concat(t, ",") + s
    end,
}
