// Command nodo-caddy is standard Caddy plus the modules monokulo-nodo needs: DNS providers for
// DNS-01 certificates and per-client rate limiting. Versions are pinned in go.mod and go.sum.
package main

import (
	caddycmd "github.com/caddyserver/caddy/v2/cmd"

	_ "github.com/caddyserver/caddy/v2/modules/standard"

	_ "github.com/caddy-dns/alidns"
	_ "github.com/caddy-dns/cloudflare"
	_ "github.com/caddy-dns/gandi"
	_ "github.com/caddy-dns/godaddy"
	_ "github.com/caddy-dns/namedotcom"
	_ "github.com/caddy-dns/porkbun"
	_ "github.com/mholt/caddy-ratelimit"
)

func main() {
	caddycmd.Main()
}
