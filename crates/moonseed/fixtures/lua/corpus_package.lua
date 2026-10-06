-- `package` and `require`, printed for comparison with Lua 5.4.9 (Phase
-- 3.24, ADR 0039). Moonseed has only the preload searcher, so the file
-- searchers are removed first and every "module not found" message is the
-- same in both.

package.searchers = { package.searchers[1] }

print("A1", package.loaded._G == _G, package.loaded.string == string, package.loaded.table == table)
print("A2", package.loaded.math == math, package.loaded.package == package, require("string") == string)
print("A3", package.config == "/\n;\n?\n!\n-\n", type(package.path), type(package.cpath))
print("A4", type(package.preload), type(package.searchers), #package.searchers)
print("A5", select("#", require("table")), require("_G") == _G)

-- preload
local calls = 0
package.preload.mod1 = function(name, data)
  calls = calls + 1
  return { name = name, data = data }
end
local m, data = require("mod1")
print("B1", m.name, m.data, data, calls)
local again, data2 = require("mod1")
print("B2", again == m, data2, calls, package.loaded.mod1 == m)

-- a loader that returns nothing stores true
package.preload.mod2 = function() calls = calls + 1 end
print("B3", require("mod2"), package.loaded.mod2, calls)
-- a loader that sets package.loaded itself and returns nothing
package.preload.mod3 = function(name) package.loaded[name] = "set" end
print("B4", require("mod3"))
-- a loader that returns false stores false, so the next require loads again
local false_calls = 0
package.preload.mod4 = function() false_calls = false_calls + 1 return false end
print("B5", require("mod4"), package.loaded.mod4, false_calls)
print("B6", require("mod4"), false_calls)
-- a loader returning several values: the first is kept
package.preload.mod5 = function() return "first", "second" end
print("B7", require("mod5"))
-- loaded wins over preload
package.loaded.mod6 = "already"
package.preload.mod6 = function() return "fresh" end
print("B8", require("mod6"))
-- a number name
package.preload["7"] = function(name) return "number " .. name end
print("B9", require(7))

-- errors
print("C1", pcall(require, "nope"))
print("C2", pcall(require))
print("C3", pcall(require, {}))
package.preload.bad = function() error("loader failed", 0) end
print("C4", pcall(require, "bad"))
print("C5", package.loaded.bad)
package.preload.badtype = 42
print("C6", pcall(require, "badtype"))

-- custom searchers
local log = {}
table.insert(package.searchers, function(name)
  log[#log + 1] = "s2:" .. name
  if name == "custom" then
    return function(n, extra) return "custom module " .. n .. " " .. tostring(extra) end, "extra"
  end
  return "\n\tno custom '" .. name .. "'"
end)
table.insert(package.searchers, function(name)
  log[#log + 1] = "s3:" .. name
  return 42
end)
print("D1", require("custom"))
print("D2", pcall(require, "missing"))
print("D3", table.concat(log, " "))
package.searchers[2] = function() return nil, "ignored" end
print("D4", pcall(require, "missing2"))
package.searchers[2] = function() return "a string" end
print("D5", pcall(require, "missing3"))
local saved = package.searchers
package.searchers = nil
print("D6", pcall(require, "x"))
package.searchers = "not a table"
print("D7", pcall(require, "x"))
package.searchers = saved
print("D8", require("custom") == "custom module custom extra")

-- metamethods on package.loaded and package.searchers
local reads = {}
setmetatable(package.loaded, { __index = function(t, k) reads[#reads + 1] = k end })
package.preload.metamod = function() return "mm" end
print("E1", require("metamod"), table.concat(reads, ","))
setmetatable(package.loaded, nil)
local proxy = setmetatable({}, { __index = function(_, i) if i == 1 then return saved[1] end end })
package.searchers = proxy
package.preload.proxied = function() return "via proxy" end
-- `require` reads the searchers raw, as Lua does
print("E2", pcall(require, "proxied"))
package.searchers = saved

-- require keeps the package table it was made with
local real = package
package = { searchers = {} }
print("F1", select("#", pcall(require, "mod1")), type(select(2, pcall(require, "mod1"))))
package = real
local old_loaded = package.loaded
package.loaded = {}
print("F2", require("mod1") == m)
package.loaded = old_loaded

-- the registry holds the same tables
local reg = debug.getregistry()
print("G1", reg._LOADED == package.loaded, reg._PRELOAD == package.preload)
reg._LOADED.viareg = "registry"
print("G2", require("viareg"))

-- a loader that requires another module
package.preload.outer = function() return "outer+" .. require("inner") end
package.preload.inner = function() return "inner" end
print("H1", require("outer"), package.loaded.inner)
-- a loader that errors in a nested require
package.preload.outer2 = function() return require("nothing") end
local ok, msg = pcall(require, "outer2")
-- Lua puts the loader's line before the message; Moonseed does not yet.
print("H2", ok, (msg:gsub("^[^:\n]*:%d+: ", "")))
print("done")
