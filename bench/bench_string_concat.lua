return {
    name = "字符串拼接",
    full = 2000000, quick = 200000,
    fn = function(n)
        local acc = ""
        for i = 1, n do
            acc = acc .. "abcdefghij"
            if #acc >= 8192 then acc = "" end
        end
        for i = 1, n do
            string.format("%d-%s-%.3f", i, "key", i * 1.5)
        end
    end,
}
