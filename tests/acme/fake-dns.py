#!/usr/bin/env python3
"""A fake DNS provider and a fake home router resolver for the ACME tests, on one address.

HTTPS: the Cloudflare API endpoints that nodo-ddns and Caddy's caddy-dns/cloudflare module use,
plus the IP echo services nodo-ddns asks. Records live in memory and are copied into
pebble-challtestsrv, which is the public DNS that Pebble validates against.

UDP 53: the resolver this device uses, as a home router would be. It answers SOA and NS for the
zone itself, which Caddy needs to find the zone and to check that a DNS-01 record has spread,
and passes every other question on to challtestsrv. While FLAG_HIDE_PRIVATE exists it drops
answers that hold private addresses, as routers with DNS rebinding protection do.

Usage: fake-dns.py ADDRESS CERT KEY TOKEN CHALLTESTSRV PUBLIC_IP STATE_DIR
"""

import http.server
import json
import ipaddress
import os
import socket
import socketserver
import ssl
import struct
import sys
import threading
import urllib.parse
import urllib.request

ADDRESS, CERT, KEY, TOKEN, CHALLTESTSRV, PUBLIC_IP, STATE = sys.argv[1:8]
ZONE = "example.com"
ZONE_ID = "023e105f4ecef8ad9ca31a8372d0c353"
NS_NAME = "ns1." + ZONE
FLAG_HIDE_PRIVATE = os.path.join(STATE, "hide-private")
ECHO_HOSTS = {"api.ipify.org", "ipv4.icanhazip.com", "checkip.amazonaws.com", "v4.ident.me", "ipv4.wtfismyip.com"}

lock = threading.Lock()
records = {}
next_id = [1]


def log(line):
    with open(os.path.join(STATE, "api.log"), "a") as f:
        f.write(line + "\n")


def challtestsrv(path, body):
    request = urllib.request.Request(
        "http://%s:8055/%s" % (CHALLTESTSRV, path), data=json.dumps(body).encode(), method="POST"
    )
    urllib.request.urlopen(request, timeout=5).read()


def sync(name, kind):
    """Makes challtestsrv hold exactly the records of this name and type."""
    fqdn = name.rstrip(".") + "."
    values = [r["content"] for r in records.values() if r["name"] == name.rstrip(".") and r["type"] == kind]
    log(json.dumps({"record": name.rstrip("."), "type": kind, "values": values}))
    if kind == "A":
        challtestsrv("clear-a", {"host": fqdn})
        if values:
            challtestsrv("add-a", {"host": fqdn, "addresses": values})
    elif kind == "TXT":
        challtestsrv("clear-txt", {"host": fqdn})
        for value in values:
            challtestsrv("set-txt", {"host": fqdn, "value": value.strip('"')})


class Api(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def ok(self, result):
        count = len(result) if isinstance(result, list) else 1
        info = {"page": 1, "per_page": 100, "count": count, "total_count": count, "total_pages": 1}
        self.reply(200, {"success": True, "errors": [], "messages": [], "result": result, "result_info": info})

    def handle_any(self, method):
        host = (self.headers.get("Host") or "").split(":")[0]
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}") if length else {}
        if host in ECHO_HOSTS:
            return self.reply(200, (PUBLIC_IP + "\n").encode(), "text/plain")
        if host != "api.cloudflare.com":
            return self.reply(404, {"success": False, "errors": [{"code": 7003, "message": "no such host"}]})
        log(json.dumps({"method": method, "path": url.path, "query": query, "body": body}))
        if self.headers.get("Authorization") != "Bearer " + TOKEN:
            return self.reply(403, {"success": False, "errors": [{"code": 10000, "message": "Authentication error"}]})
        path = url.path.removeprefix("/client/v4")
        with lock:
            if method == "GET" and path == "/zones":
                zones = [{"id": ZONE_ID, "name": ZONE}] if query.get("name", "").rstrip(".") == ZONE else []
                return self.ok(zones)
            base = "/zones/%s/dns_records" % ZONE_ID
            if path == base and method == "GET":
                found = []
                for record in records.values():
                    name = query.get("name.exact") or query.get("name")
                    if name and record["name"] != name.rstrip("."):
                        continue
                    if query.get("type") and record["type"] != query["type"]:
                        continue
                    if query.get("content.exact") and record["content"] != query["content.exact"]:
                        continue
                    if query.get("content.contains") and query["content.contains"] not in record["content"]:
                        continue
                    found.append(record)
                return self.ok(found)
            if path == base and method == "POST":
                # Like Cloudflare, names may come relative to the zone, as caddy-dns sends them.
                name = body["name"].rstrip(".")
                if name != ZONE and not name.endswith("." + ZONE):
                    name += "." + ZONE
                record = {
                    "id": "rec%d" % next_id[0],
                    "type": body["type"],
                    "name": name,
                    "content": body["content"],
                    "ttl": body.get("ttl", 1),
                    "zone_id": ZONE_ID,
                    "zone_name": ZONE,
                }
                next_id[0] += 1
                records[record["id"]] = record
                sync(record["name"], record["type"])
                return self.ok(record)
            if path.startswith(base + "/"):
                record = records.get(path[len(base) + 1:])
                if record is None:
                    return self.reply(404, {"success": False, "errors": [{"code": 81044, "message": "Record does not exist."}]})
                if method == "PATCH":
                    record.update({k: v for k, v in body.items() if k in ("content", "ttl")})
                    sync(record["name"], record["type"])
                    return self.ok(record)
                if method == "DELETE":
                    del records[record["id"]]
                    sync(record["name"], record["type"])
                    return self.ok({"id": record["id"]})
        return self.reply(404, {"success": False, "errors": [{"code": 7000, "message": "No route"}]})

    def do_GET(self):
        self.handle_any("GET")

    def do_POST(self):
        self.handle_any("POST")

    def do_PATCH(self):
        self.handle_any("PATCH")

    def do_DELETE(self):
        self.handle_any("DELETE")


class ThreadingHttps(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def read_name(packet, offset):
    labels = []
    while True:
        length = packet[offset]
        offset += 1
        if length == 0:
            return ".".join(labels).lower(), offset
        labels.append(packet[offset:offset + length].decode())
        offset += length


def encode_name(name):
    out = b""
    for label in name.rstrip(".").split("."):
        out += bytes([len(label)]) + label.encode()
    return out + b"\0"


def answer(query, qname, qtype, rrs, rcode=0):
    """A reply with the question copied and `rrs` as (type, rdata) records for qname."""
    ident, flags = struct.unpack(">HH", query[:4])
    end = 12
    _, end = read_name(query, end)
    question = query[12:end + 4]
    head = struct.pack(">HHHHHH", ident, 0x8180 | (flags & 0x0100) | rcode, 1, len(rrs), 0, 0)
    body = b""
    for kind, rdata in rrs:
        body += encode_name(qname) + struct.pack(">HHIH", kind, 1, 60, len(rdata)) + rdata
    return head + question + body


def has_private_a(reply):
    """Whether a reply from challtestsrv, which never compresses names, holds a private address."""
    _, _, qd, an, _, _ = struct.unpack(">HHHHHH", reply[:12])
    offset = 12
    for _ in range(qd):
        _, offset = read_name(reply, offset)
        offset += 4
    for _ in range(an):
        _, offset = read_name(reply, offset)
        kind, _, _, length = struct.unpack(">HHIH", reply[offset:offset + 10])
        offset += 10
        if kind == 1 and length == 4 and ipaddress.ip_address(reply[offset:offset + 4]).is_private:
            return True
        offset += length
    return False


SOA, NS, A = 6, 2, 1


def serve_dns():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((ADDRESS, 53))
    upstream = (CHALLTESTSRV, 53)
    while True:
        query, client = sock.recvfrom(4096)
        try:
            qname, end = read_name(query, 12)
            qtype = struct.unpack(">H", query[end:end + 2])[0]
            if qname == ZONE and qtype == SOA:
                rdata = encode_name(NS_NAME) + encode_name("hostmaster." + ZONE) + struct.pack(">IIIII", 1, 60, 60, 600, 60)
                reply = answer(query, qname, qtype, [(SOA, rdata)])
            elif qtype == SOA:
                reply = answer(query, qname, qtype, [])
            elif qname == ZONE and qtype == NS:
                reply = answer(query, qname, qtype, [(NS, encode_name(NS_NAME))])
            elif qname == NS_NAME and qtype == A:
                reply = answer(query, qname, qtype, [(A, socket.inet_aton(ADDRESS))])
            else:
                relay = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                relay.settimeout(3)
                relay.sendto(query, upstream)
                reply, _ = relay.recvfrom(4096)
                relay.close()
                if qtype == A and os.path.exists(FLAG_HIDE_PRIVATE) and has_private_a(reply):
                    log(json.dumps({"dns": "hid a private answer", "name": qname}))
                    reply = answer(query, qname, qtype, [])
            sock.sendto(reply, client)
        except Exception as error:  # A test fake: one bad packet must not stop the resolver.
            log(json.dumps({"dns_error": str(error)}))


threading.Thread(target=serve_dns, daemon=True).start()
server = ThreadingHttps((ADDRESS, 443), Api)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(CERT, KEY)
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
