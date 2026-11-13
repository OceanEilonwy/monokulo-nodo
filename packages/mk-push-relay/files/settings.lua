-- The push relay's settings. The relay's own names are in brackets in each help text.
--
-- Every value reaches the launcher as a NODO_* variable from a line-oriented file, which cannot
-- hold a line break, so the two key files are taken on one line and the launcher puts the APNs
-- key back into PEM form.
return {
  api_version = 1,
  settings = {
    {
      name = "public_uri",
      type = "url",
      label = "Public address",
      help = "The https address apps and senders reach this relay at, such as "
        .. "https://push.example.org: the address this package's site is published at "
        .. "(server.public_uri). Every endpoint the relay hands out is under it.",
      required = true,
      validate = function(url)
        return nodo.matches(url, "^https://[A-Za-z0-9.-]+(:[0-9]+)?/?$"),
          "must be an https:// origin with no path"
      end,
    },
    {
      name = "app_name",
      type = "string",
      label = "App name",
      help = "The name apps register with (a key of handles.apps). Monokular registers as "
        .. "monokular.",
      required = true,
      default = "monokular",
      validate = function(name)
        return nodo.matches(name, "^[a-z0-9]([a-z0-9-]{0,62}[a-z0-9])?$"),
          "must be lowercase letters, digits and hyphens"
      end,
    },
    {
      name = "apns_topic",
      type = "string",
      label = "iOS bundle ID",
      help = "The app's bundle ID, for APNs (apns_topic). Leave empty if the relay serves no "
        .. "iPhones.",
      validate = function(topic)
        return nodo.matches(topic, "^[A-Za-z0-9.-]{1,155}$"), "must be a bundle ID such as com.example.app"
      end,
    },
    {
      name = "fcm_project_id",
      type = "string",
      label = "Firebase project ID",
      help = "The app's Firebase project ID, for FCM (fcm_project_id). Leave empty if the relay "
        .. "serves no Android phones.",
      validate = function(id)
        return nodo.matches(id, "^[a-z0-9-]{1,63}$"), "must be a Firebase project ID"
      end,
    },
    {
      name = "apns_team_id",
      type = "string",
      label = "Apple team ID",
      help = "The Apple Developer team the APNs key belongs to: 10 letters and digits "
        .. "(apns.team_id). Needed with the APNs key.",
      validate = function(id)
        return nodo.matches(id, "^[A-Z0-9]{10}$"), "must be 10 capital letters and digits"
      end,
    },
    {
      name = "apns_key_id",
      type = "string",
      label = "APNs key ID",
      help = "The <id> of the key's AuthKey_<id>.p8 file: 10 letters and digits (apns.key_id). "
        .. "Needed with the APNs key.",
      validate = function(id)
        return nodo.matches(id, "^[A-Z0-9]{10}$"), "must be 10 capital letters and digits"
      end,
    },
    {
      name = "apns_sandbox",
      type = "bool",
      label = "APNs sandbox",
      help = "Send to APNs's development service, for apps built for development "
        .. "(apns.endpoint https://api.sandbox.push.apple.com).",
      default = false,
    },
    {
      name = "apns_key",
      type = "secret",
      label = "APNs key",
      help = "The AuthKey_<id>.p8 file's contents on one line: the BEGIN and END lines may stay "
        .. "or go. Without it the relay hands nothing to APNs and refuses iOS registrations.",
      validate = function(key)
        return nodo.matches(key, "^(-----BEGIN PRIVATE KEY-----)?[ A-Za-z0-9+/=]+(-----END PRIVATE KEY-----)?[ ]*$"),
          "must be the .p8 key's base64 on one line"
      end,
    },
    {
      name = "fcm_credentials",
      type = "secret",
      label = "Firebase service account",
      help = "The service account's JSON key file, on one line (its private key keeps its \\n "
        .. "escapes). Without it the relay hands nothing to FCM and refuses Android "
        .. "registrations.",
      validate = function(json)
        return nodo.matches(json, "^[ ]*\\{.*\\}[ ]*$"), "must be the JSON key file on one line"
      end,
    },
    {
      name = "handle_key",
      type = "secret",
      label = "Handle key",
      help = "Every handle's id is made under this key (handles.id_key). The device creates it "
        .. "on install. Changing it gives every device a new endpoint at its next registration.",
      required = true,
      generate = { hex = 32 },
      validate = function(key)
        return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 hex characters"
      end,
    },
  },
}
