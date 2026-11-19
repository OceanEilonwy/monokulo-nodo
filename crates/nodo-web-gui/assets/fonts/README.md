# The web GUI's fonts

The browser pages use Inter for text and JetBrains Mono for addresses, keys and logs. Both are
compiled into `nodo-web-gui` (`src/assets.rs`) and served by the Nodo itself under hashed names,
so the pages look the same offline and the content security policy needs only `font-src 'self'`.

Each file is the Latin subset of the font's variable build (one file covers every weight),
unchanged, from the Fontsource npm packages:

| File | Package | Source | SHA-256 |
| --- | --- | --- | --- |
| `inter-latin-wght-normal.woff2` | `@fontsource-variable/inter` 5.3.0 | https://cdn.jsdelivr.net/npm/@fontsource-variable/inter@5.3.0/files/inter-latin-wght-normal.woff2 | `3100e775e8616cd2611beecfa23a4263d7037586789b43f035236a2e6fbd4c62` |
| `jetbrains-mono-latin-wght-normal.woff2` | `@fontsource-variable/jetbrains-mono` 5.3.0 | https://cdn.jsdelivr.net/npm/@fontsource-variable/jetbrains-mono@5.3.0/files/jetbrains-mono-latin-wght-normal.woff2 | `18be452724bfdc236c074ca94a249a7f41a86752c7d04ab258ce9ed5651f6a7e` |

Both are under the SIL Open Font License 1.1. Their licences are beside them, as shipped in each
package (`Inter-LICENSE.txt`, `JetBrainsMono-LICENSE.txt`); `packages/nodo-tools/template`
installs Inter's as `/usr/share/licenses/nodo-tools/OFL-Inter.txt`, next to the JetBrains Mono
licence the built-in screen already installs.

Characters outside the Latin subset fall back to the system fonts named after these in
`app.css`.
