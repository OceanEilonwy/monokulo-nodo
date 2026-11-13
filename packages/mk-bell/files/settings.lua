-- The Monokular Bell's settings. The Bell's own names are in brackets in each help text.
--
-- An integration (a point of sale's environment) is on when its application ID is set, and then
-- its secret must be set too; the Bell refuses to start otherwise, and with no integration on.

local settings = {
  {
    name = "public_uri",
    type = "url",
    label = "Public address",
    help = "The https address points of sale and tills reach this Bell at, such as "
      .. "https://bell.example.org: the address this package's site is published at "
      .. "(bell.public_uri). Webhooks go to it followed by /v1/<point of sale>/<environment>/webhook.",
    required = true,
    validate = function(url)
      return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?/?$"),
        "must be an https:// origin with no path"
    end,
  },
  {
    name = "vapid_key",
    type = "secret",
    label = "VAPID key",
    help = "Signs every ding (bell.vapid_key). The device creates it on install. The tills' "
      .. "subscriptions are made for its public key, so a new one silences every till until it "
      .. "registers again.",
    required = true,
    generate = { hex = 32 },
    validate = function(key)
      return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 hex characters"
    end,
  },
}

local function secret_line(value)
  return nodo.matches(value, "^[\\x21-\\x7e]{1,1024}$"), "must be printable characters with no spaces"
end

local function square(env, label)
  table.insert(settings, {
    name = "square_" .. env .. "_application_id",
    type = "string",
    label = "Square " .. label .. " application ID",
    help = "The Square " .. label .. " application the tills sign in through, from the Developer "
      .. "Console (bell.square." .. env .. ".application_id). Set, the Bell takes its webhooks at "
      .. "/v1/square/" .. env .. "/webhook.",
    validate = function(id)
      return nodo.matches(id, "^(sandbox-)?sq0id[a-z]-[A-Za-z0-9_-]{1,200}$"), "must be a Square application ID"
    end,
  })
  table.insert(settings, {
    name = "square_" .. env .. "_webhook_key",
    type = "secret",
    label = "Square " .. label .. " webhook key",
    help = "The signature key of the " .. label .. " application's webhook subscription, from the "
      .. "Developer Console's Webhooks page. Needed when, and only when, its application ID is set.",
    validate = secret_line,
  })
end

local function clover(env, label)
  table.insert(settings, {
    name = "clover_" .. env .. "_application_id",
    type = "string",
    label = "Clover " .. label .. " App ID",
    help = "The Clover " .. label .. " app the tills sign in through: its App ID, from the "
      .. "developer dashboard (bell.clover." .. env .. ".application_id). Set, the Bell takes its "
      .. "webhooks at /v1/clover/" .. env .. "/webhook.",
    validate = function(id)
      return nodo.matches(id, "^[A-Za-z0-9]{13}$"), "must be 13 letters and digits"
    end,
  })
  table.insert(settings, {
    name = "clover_" .. env .. "_auth_code",
    type = "secret",
    label = "Clover " .. label .. " Auth Code",
    help = "The " .. label .. " app's Auth Code, from its webhook settings. Needed when, and only "
      .. "when, its App ID is set.",
    validate = secret_line,
  })
end

square("production", "production")
square("sandbox", "sandbox")
clover("north_america", "North America")
clover("europe", "Europe")
clover("latin_america", "Latin America")
clover("sandbox", "sandbox")

return { api_version = 1, settings = settings }
