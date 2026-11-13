return {
  api_version = 1,
  settings = {
    {
      name = "daemon_url",
      type = "url",
      label = "Monero node",
      help = "The RPC address of the node Monokulo watches the chain with. By default it is the "
        .. "node on this device.",
      required = true,
      default = "$(monero-daemon.public_rpc_url)",
      validate = function(url)
        return nodo.matches(url, "^https?://[^/]+/?$"),
          "must be http:// or https:// with no path, because Monokulo reaches a node at its root"
      end,
    },
    {
      name = "encryption_key",
      type = "secret",
      label = "Encryption key",
      help = "Encrypts each store's engine token in Monokulo's database. The device creates it "
        .. "on install. It can never change once a store is connected, so it is kept with "
        .. "Monokulo's data and backed up with it.",
      required = true,
      generate = { hex = 32 },
      validate = function(key)
        return nodo.matches(key, "^[0-9a-f]{64}$"), "must be 64 hex characters"
      end,
    },
  },
}
