-- The base system's device panel: temperature, storage, memory and uptime.
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

return {
  panel = function(d)
    local out = { rows = {}, status = "ok" }
    if d.temp_c then
      out.big = math.floor(d.temp_c) .. " °C"
      if d.temp_c >= 85 then out.status = "warn" end
      out.sample = d.temp_c
    else
      out.big = "--"
      out.status = "idle"
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
