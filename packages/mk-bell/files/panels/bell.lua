-- The Bell's registrations and integrations, from its own GET /status on its status listener
-- (crates/monokulo-bell/src/status.rs): registrations.stored, points_of_sale (each point of sale's
-- environments, "ready" or "off"), and last_ding, null until a ding was sent since the Bell
-- started, else { at, outcome } with outcome one of sent, gone, failed. The screen gives no clock,
-- so the time of the last ding is not shown.

local integrations = {
  { "square", "production", "Square" },
  { "square", "sandbox", "Square sandbox" },
  { "clover", "north_america", "Clover NA" },
  { "clover", "europe", "Clover EU" },
  { "clover", "latin_america", "Clover LA" },
  { "clover", "sandbox", "Clover sandbox" },
}

local said = { ["sent"] = "sent", ["gone"] = "till gone", ["failed"] = "failed" }

return {
  panel = function(d)
    local out = { rows = {} }
    local stored = d.registrations and d.registrations.stored
    if stored then
      out.big = tostring(math.floor(stored))
      out.unit = stored == 1 and "till" or "tills"
      out.sample = stored
    else
      out.big = "--"
      out.unit = "tills"
    end
    local pos = d.points_of_sale or {}
    local ready = {}
    for _, i in ipairs(integrations) do
      local environments = pos[i[1]]
      if type(environments) == "table" and environments[i[2]] == "ready" then
        table.insert(ready, i[3])
      end
    end
    if #ready > 0 then
      local names = table.concat(ready, ", ")
      -- A row's value holds at most 40 characters.
      if #names > 40 then
        names = #ready .. " points of sale"
      end
      table.insert(out.rows, { "serving", names })
    end
    local last = d.last_ding
    if last and last.outcome then
      table.insert(out.rows, { "last ding", said[last.outcome] or tostring(last.outcome) })
    end
    if #ready == 0 then
      out.status = "warn"
      out.note = "no point of sale is set up"
    elseif last and last.outcome == "failed" then
      out.status = "warn"
      out.note = "the last ding was not taken"
    elseif not stored then
      out.status = "warn"
      out.note = "could not count the registrations"
    elseif stored == 0 then
      out.status = "idle"
      out.note = "no till has registered yet"
    else
      out.status = "ok"
    end
    return out
  end,
}
