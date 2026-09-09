return {
    name = "浮点算术",
    full = 10000000, quick = 1000000,
    fn = function(n)
        local s = 0.0
        for i = 1, n do
            s = s + math.sin(i) * math.cos(i) + math.sqrt(i % 1000)
        end
        return s
    end,
}
