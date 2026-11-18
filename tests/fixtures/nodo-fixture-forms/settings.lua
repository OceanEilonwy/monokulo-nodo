-- A deliberately awkward settings form, for the TUI's tests and the VM journeys. It is far taller
-- than any terminal and has every setting type: required and optional, with and without
-- defaults, validators that refuse an empty string, very long labels and help, unicode, labels
-- that start another label ("Email", "Email from"), help that names other fields, and references
-- to another package's public settings. Modelled on the first real third-party app, handicrafts,
-- whose 27 settings broke a form that had only seen 10. Keep every setting type covered.
local function email(value)
  return nodo.matches(value, "^[^@ ]+@[^@ ]+\\.[^@ ]+$"), "must be an email address"
end

local function web_address(url)
  return nodo.matches(url, "^https?://[A-Za-z0-9.\\[\\]:-]+$"),
    "must be http:// or https:// with no path or trailing slash"
end

return {
  api_version = 1,
  settings = {
    -- The shop
    {
      name = "public_url",
      type = "url",
      label = "Public address",
      help = "The shop's address, used in links in emails. Give the site this address in Hosting.",
      required = true,
      default = "https://forms.example.com",
      validate = web_address,
      public = "shop-url",
    },
    {
      -- Required with no default: a fresh install cannot be saved until it is filled in.
      name = "shop_name",
      type = "string",
      label = "Shop name",
      required = true,
      validate = function(v)
        return #v > 0 and #v <= 60, "must be 1 to 60 characters"
      end,
    },
    {
      name = "tagline",
      type = "string",
      label = "Tagline · ünïcödé ✓",
      help = "Shown under the name. Café, crème brûlée, 日本語 and emoji such as 🧶 all work.",
      default = "Handmade in Ōtautahi — ¡hola!",
    },
    {
      name = "admin_user",
      type = "string",
      label = "Admin user",
      help = "The user name for /admin.",
      required = true,
      default = "owner",
      validate = function(user)
        return nodo.matches(user, "^[^: ]{1,64}$"), "must be up to 64 characters, no spaces or colons"
      end,
    },
    {
      name = "admin_password",
      type = "secret",
      label = "Admin password",
      help = "At least 8 characters. Until it is set, /admin stays closed.",
      validate = function(password)
        return nodo.matches(password, "^.{8,}$"), "must be at least 8 characters"
      end,
    },
    {
      -- Made by the device when the app is installed, and kept.
      name = "session_key",
      type = "secret",
      label = "Session key",
      help = "Signs sign-in cookies. Made for you when the app is installed.",
      required = true,
      generate = { hex = 32 },
    },

    -- Email: "Email" starts "Email from", and the help of the fields below names Email and SMTP.
    {
      name = "mail",
      type = "enum",
      label = "Email",
      help = "off: emails wait in the outbox. smtp or tuta: they are sent. log: written to the log.",
      choices = { "off", "smtp", "tuta", "log" },
      default = "off",
    },
    {
      name = "mail_from",
      type = "string",
      label = "Email from",
      help = "Empty uses the SMTP user. Only used when Email is smtp or tuta.",
      validate = email,
    },
    {
      name = "notify_to",
      type = "string",
      label = "Notifications to",
      help = "Where order notifications go. Needs Email to be on; Email from is the sender.",
      validate = email,
    },
    {
      name = "smtp_host",
      type = "string",
      label = "SMTP server",
      help = "Such as smtp.example.com. SMTP port, SMTP user and SMTP password go with it.",
      validate = function(host)
        return nodo.matches(host, "^[A-Za-z0-9.-]+$"), "must be a host name"
      end,
    },
    {
      -- An optional int with no default: empty must save.
      name = "smtp_port",
      type = "int",
      label = "SMTP port",
      help = "Empty: 465 for tls, 587 for starttls, 25 for none.",
      min = 1,
      max = 65535,
    },
    {
      name = "smtp_security",
      type = "enum",
      label = "SMTP security",
      choices = { "tls", "starttls", "none" },
      default = "tls",
    },
    {
      name = "smtp_user",
      type = "string",
      label = "SMTP user",
      help = "Empty if the server needs no login.",
    },
    {
      name = "smtp_password",
      type = "secret",
      label = "SMTP password",
    },
    {
      -- An optional enum with no default.
      name = "log_level",
      type = "enum",
      label = "Log level",
      choices = { "error", "warn", "info", "debug", "trace" },
    },

    -- Numbers of every shape
    {
      name = "workers",
      type = "int",
      label = "Workers",
      help = "Requests handled at once.",
      required = true,
      default = 4,
      min = 1,
      max = 64,
    },
    {
      -- Required, no default, bounded: empty is refused.
      name = "daily_limit",
      type = "int",
      label = "Orders per day",
      help = "Orders the whole shop takes in a day.",
      required = true,
      min = 1,
      max = 100000,
    },
    {
      name = "retries",
      type = "int",
      label = "Retries",
      help = "No bounds and no default.",
    },
    {
      name = "clock_offset",
      type = "int",
      label = "Clock offset (minutes)",
      help = "A minimum only, and below zero.",
      min = -720,
    },
    {
      name = "max_upload_mib",
      type = "int",
      label = "Largest upload, MiB",
      help = "A maximum only.",
      default = 10,
      max = 2048,
    },

    -- Yes or no
    {
      name = "signup",
      type = "bool",
      label = "Customers can make accounts",
      default = false,
    },
    {
      name = "maintenance",
      type = "bool",
      label = "Maintenance mode",
      help = "No default: off until it is turned on.",
    },
    {
      name = "theme",
      type = "enum",
      label = "Theme",
      help = "Required, with no default.",
      required = true,
      choices = { "light", "dark", "high contrast" },
    },

    -- Monero, and other apps' settings
    {
      name = "payout_address",
      type = "monero-address",
      label = "Payout address",
      help = "Where payments go. Empty keeps them in the wallet.",
    },
    {
      -- Another app's public setting, as the whole default.
      name = "daemon_url",
      type = "url",
      label = "Monero node",
      help = "A node's RPC address. The default is this Nodo's own node.",
      default = "$(cuprated.public_rpc_url)",
    },
    {
      -- Text and a reference together.
      name = "rpc_endpoint",
      type = "url",
      label = "Node JSON-RPC endpoint",
      default = "http://127.0.0.1:$(cuprated.rpc-port)/json_rpc",
    },
    {
      name = "rpc_port",
      type = "int",
      label = "Node RPC port",
      help = "Follows the node unless you give a port.",
      default = "$(cuprated.rpc-port)",
      min = 1,
      max = 65535,
    },

    -- Bank transfer, with validators that refuse an empty string
    {
      name = "bsb",
      type = "string",
      label = "BSB",
      validate = function(bsb)
        return nodo.matches(bsb, "^[0-9]{3}-?[0-9]{3}$"), "must be six digits, such as 062-000"
      end,
    },
    {
      name = "account_number",
      type = "string",
      label = "Account number",
      validate = function(number)
        return nodo.matches(number, "^[0-9 ]{4,12}$"), "must be digits"
      end,
    },

    -- Long words
    {
      name = "return_url",
      type = "url",
      -- As long as a label may be (80 bytes): wider than the card at 60 and 80 columns.
      label = "Where customers return after paying on the payment provider's own checkout page",
      help = "This label is longer than any terminal is wide, and so is this help: it has to wrap "
        .. "under the label without hiding the value beside it or pushing the + reference link off "
        .. "the card. Customers who pay by card or by Monero both come back here, so it must be "
        .. "the public address of the shop followed by /paid, or the shop will not see payments "
        .. "that finished while the customer's browser was closed. Email and SMTP settings are not "
        .. "involved.",
      default = "https://forms.example.com",
    },
    {
      name = "footer_note",
      type = "string",
      label = "Footer",
      help = "Averyveryverylongwordwithnospacesthatcannotwrapanywhereatallbutstillhastofitsomehowintoacard.",
    },
    {
      name = "api_token",
      type = "secret",
      label = "API token",
      help = "Optional, made for you when the app is installed.",
      generate = { hex = 16 },
    },
    {
      name = "webhook_secret",
      type = "secret",
      label = "Webhook secret",
      validate = function(key)
        return nodo.matches(key, "^whsec_[A-Za-z0-9]+$"), "must start with whsec_"
      end,
    },
    {
      name = "analytics_url",
      type = "url",
      label = "Analytics",
      help = "Optional, with no default.",
    },
    {
      name = "last_field",
      type = "string",
      label = "Last field",
      help = "The last field, so a test can tell it reached the end.",
      default = "the end",
    },
  },
}
