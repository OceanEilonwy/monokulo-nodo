-- Settings a user may change for cuprated. Ports and listen addresses are fixed, because the
-- firewall rules in cuprated-service are written for them. RPC is restricted and on loopback
-- only, so there is no setting to put it on the network.
return {
  api_version = 1,
  settings = {
    { name = "outbound_peers", type = "int", label = "Outbound peers", default = 32, min = 1, max = 128,
      help = "Connections this node opens to other nodes." },
    { name = "inbound_peers", type = "int", label = "Inbound peer limit", default = 64, min = 0, max = 512,
      help = "Most connections other nodes may open to this one." },
    { name = "accept_inbound", type = "bool", label = "Accept inbound peers", default = true },
    { name = "log_level", type = "enum", label = "Log level", default = "info",
      choices = { "error", "warn", "info", "debug" } },
    { name = "rpc_port", type = "int", label = "Local RPC port", default = 18081, min = 18081, max = 18081,
      public = "rpc-port", endpoint = "tcp", help = "Fixed. Other packages reference it." },
    -- The port of the restricted RPC, which is the only RPC cuprated serves. The helper forwards
    -- the node's onion address only to a port a node exports under this name.
    { name = "restricted_rpc_port", type = "int", label = "Restricted RPC port", default = 18081,
      min = 18081, max = 18081, public = "restricted-rpc-port",
      help = "Fixed. RPC is always restricted and on loopback only." },
    -- The node's RPC as a URL, which apps such as Monokulo reference through the role.
    { name = "rpc_url", type = "url", label = "Local RPC address", default = "http://127.0.0.1:18081",
      public = "public_rpc_url", endpoint = "tcp", help = "Fixed. Other packages reference it.",
      validate = function(url) return url == "http://127.0.0.1:18081", "this address is fixed" end },
  },
}
