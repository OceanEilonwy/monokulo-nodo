-- The push relay's handles and readiness, from its own GET /status on its status listener
-- (crates/push-relay/src/status.rs): handles.stored, platforms.apns and platforms.fcm ("ready" or
-- "not-configured"), and last_delivery, null until a push was handed on since the relay started,
-- else { at, outcome } with outcome one of sent, gone, refused, unavailable, too-large. The
-- screen gives no clock, so the time of the last push is not shown.

local said = {
  ["sent"] = "sent",
  ["gone"] = "device gone",
  ["refused"] = "refused",
  ["unavailable"] = "service busy",
  ["too-large"] = "too large",
}

local function ready(value)
  return value == "ready"
end

return {
  panel = function(d)
    local platforms = d.platforms or {}
    local apns, fcm = ready(platforms.apns), ready(platforms.fcm)
    local stored = d.handles and d.handles.stored
    local out = { rows = {} }
    if stored then
      out.big = tostring(math.floor(stored))
      out.unit = stored == 1 and "handle" or "handles"
      out.sample = stored
    else
      out.big = "--"
      out.unit = "handles"
    end
    table.insert(out.rows, { "APNs", apns and "ready" or "not set up" })
    table.insert(out.rows, { "FCM", fcm and "ready" or "not set up" })
    local last = d.last_delivery
    if last and last.outcome then
      table.insert(out.rows, { "last push", said[last.outcome] or tostring(last.outcome) })
    end
    if not apns and not fcm then
      out.status = "warn"
      out.note = "no APNs or FCM credentials set"
    elseif last and (last.outcome == "refused" or last.outcome == "unavailable") then
      out.status = "warn"
      out.note = "the last push was not taken"
    elseif not stored then
      out.status = "warn"
      out.note = "could not count the handles"
    else
      out.status = "ok"
    end
    return out
  end,
}
