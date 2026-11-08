-- Settings a user may change for cuprated. Ports and listen addresses are fixed, because the
-- firewall rules in cuprated-service are written for them.
return {
  api_version = 1,
  settings = {
    { name = "outbound_peers", type = "int", label = "Outbound peers", default = 32, min = 1, max = 128,
      help = "Connections this node opens to other nodes." },
    { name = "inbound_peers", type = "int", label = "Inbound peer limit", default = 64, min = 0, max = 512,
      help = "Most connections other nodes may open to this one." },
    { name = "accept_inbound", type = "bool", label = "Accept inbound peers", default = true },
    { name = "restricted_rpc", type = "bool", label = "Restricted RPC on the network", default = false,
      help = "Lets wallets on your network use this node on port 18089." },
    { name = "log_level", type = "enum", label = "Log level", default = "info",
      choices = { "error", "warn", "info", "debug" } },
    { name = "rpc_port", type = "int", label = "Local RPC port", default = 18081, min = 18081, max = 18081,
      public = "rpc-port", endpoint = "tcp", help = "Fixed. Other packages reference it." },
  },
}
