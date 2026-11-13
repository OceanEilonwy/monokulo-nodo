-- The base system's device panel: chip and SSD temperature, storage, memory and uptime.
-- Every value is optional, because the screen leaves out what it cannot read.

local function bytes(n)
  local units = { "B", "KB", "MB", "GB", "TB" }
  local i = 1
  while n >= 1000 and i < #units do
    n = n / 1000
    i = i + 1
  end
  if n >= 100 or i == 1 then
    return string.format("%d %s", math.floor(n), units[i])
  end
  return string.format("%.1f %s", n, units[i])
end

local function share(used, total)
  return string.format("%d%%", math.floor(used / total * 100 + 0.5))
end

local function uptime(secs)
  local days = secs // 86400
  if days >= 2 then return days .. " days" end
  if days == 1 then return "1 day" end
  local hours = secs // 3600
  if hours >= 1 then return hours .. " h" end
  return (secs // 60) .. " min"
end

-- The Nodo is fanless, so the kernel slows the chip from 70 °C and shuts down at 100 °C
-- (docs/THERMAL.md). The panel warns before either matters. The SSD slows its own writes at
-- about 82 °C.
local CHIP_WARN = 75
local SSD_WARN, SSD_ERROR = 70, 80

local RANK = { ok = 1, warn = 2, error = 3 }
local function worse(a, b)
  return RANK[b] > RANK[a] and b or a
end

return {
  panel = function(d)
    local out = { rows = {}, status = "ok" }
    if d.temp_c then
      out.big = math.floor(d.temp_c) .. " °C"
      if d.temp_c >= CHIP_WARN then out.status = "warn" end
      out.sample = d.temp_c
    else
      out.big = "--"
      out.status = "idle"
    end
    if d.ssd_temp_c then
      table.insert(out.rows, { "ssd temp", math.floor(d.ssd_temp_c) .. " °C" })
      local ssd = "ok"
      if d.ssd_temp_c >= SSD_ERROR then ssd = "error" elseif d.ssd_temp_c >= SSD_WARN then ssd = "warn" end
      if ssd ~= "ok" then
        out.status = out.status == "idle" and ssd or worse(out.status, ssd)
        out.note = ssd == "error" and "ssd is too hot" or "ssd is hot"
      end
    end
    if d.ssd_total and d.ssd_total > 0 then
      table.insert(out.rows, { "ssd", share(d.ssd_used, d.ssd_total) .. " of " .. bytes(d.ssd_total) })
      out.bar = d.ssd_used / d.ssd_total
    end
    if d.mem_total and d.mem_total > 0 then
      table.insert(out.rows, { "mem", bytes(d.mem_used) .. " of " .. bytes(d.mem_total) })
    end
    if d.uptime_secs then
      table.insert(out.rows, { "up", uptime(d.uptime_secs) })
    end
    return out
  end,
}
