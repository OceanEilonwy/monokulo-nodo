# Key statements for the snapshot channels

`<channel>/keys.toml` and its signatures `keys.toml.sig` are copied to
`snapshots/channels/<channel>/keys.toml{,.sig}` every time `scripts/publish-snapshot.sh publish`
runs (docs/UPDATES.md, "Key rotation").

`stable/keys.toml` (sequence 1) restates the keys and threshold pinned in
`packages/nodo-tools/files/snapshot-pins.toml`, so a device that applies it trusts exactly what it
did before. It is published only so that the file exists: GitHub Pages answers a missing file
inside a directory with HTTP 503, and the client in nodo-tools 12 (the first T6N images) treats
that as a failed download and stops the update. `crates/nodo-update/tests/shipped_key_statement.rs`
checks that the shipped pins accept it and that it changes nothing.

It was signed once, by hand, with key 1 (the key CI signs manifests with):

    cargo run -q --release -p nodo-update --bin nodo-snapshot -- sign keys feed/snapshot-keys/stable/keys.toml <key 1 file>

A real rotation replaces both files with a statement of sequence 2 or higher, signed by enough of
the currently trusted keys to meet their threshold.
