-- cuprated's sync progress, from its JSON-RPC get_info.
--
-- What cuprated 0.1.0 reports: height, target_height, database_size and version are real.
-- target_height is the peers' height while the node is behind, and 0 once it has caught up (and
-- also when it has no peers, which get_info cannot tell apart). The peer counts are always 0 for
-- now, so they are not shown.

local function thousands(n)
  local s = tostring(math.floor(n))
  local out = ""
  while #s > 3 do
    out = "," .. s:sub(-3) .. out
    s = s:sub(1, -4)
  end
  return s .. out
end

return {
  panel = function(d, history)
    local height = d.height or 0
    local target = d.target_height or 0
    local out = { rows = {}, series = history }
    if target > height then
      local pct = math.floor(height / target * 100)
      out.big = pct .. "%"
      out.unit = "synced"
      out.bar = height / target
      out.status = "busy"
      out.sample = pct
      table.insert(out.rows, { "height", thousands(height) .. " / " .. thousands(target) })
      table.insert(out.rows, { "behind", thousands(target - height) .. " blocks" })
    elseif height > 0 then
      out.big = thousands(height)
      out.unit = "blocks"
      out.status = "ok"
      out.sample = 100
      out.note = "up to date with its peers"
    else
      out.big = "--"
      out.unit = "starting"
      out.status = "idle"
    end
    if d.database_size and d.database_size > 0 then
      table.insert(out.rows, { "database", string.format("%.0f GB", d.database_size / 1e9) })
    end
    if d.tx_pool_size then
      table.insert(out.rows, { "tx pool", tostring(d.tx_pool_size) })
    end
    if d.version and d.version ~= "" and not out.note then
      out.note = "cuprated " .. d.version
    end
    return out
  end,
}
