# Sourced by xbps-src before the configure, build, check and install steps of every package, once
# scripts/_build-packages-inner.sh has started the sccache server (it copies this file into
# void-packages' common/environment/<step>/ only then). The server holds the cache settings; a
# client only needs to know to go through it.
#
# cargo runs rustc through RUSTC_WRAPPER, for build_style=cargo and for this project's own cargo
# recipes alike. If the server stops answering mid-build, IGNORE_SERVER_IO_ERROR makes sccache run
# rustc itself rather than fail the compile. sccache cannot cache incremental builds.
export RUSTC_WRAPPER=sccache SCCACHE_IGNORE_SERVER_IO_ERROR=1 CARGO_INCREMENTAL=0

# sccache 0.19 keys a Rust compile on its working directory and the CARGO_* variables, which hold
# absolute paths, and these include $wrksrc, named after the package and its version. Recipes that
# build the same upstream tree under different names (mk-pay, mk-bell and mk-push-relay all build
# monokulo-pay at one commit, as build_wrksrc) would never share an entry. So the tree they build
# moves to a path named only after build_wrksrc, and $wrksrc/$build_wrksrc becomes a link to it:
# xbps-src and the recipe still find everything where they expect, and the compiler sees the same
# absolute paths, target directory included, from every recipe that builds that tree.
# _build-packages-inner.sh removes the moved tree after each package.
if [ -n "$build_wrksrc" ] && [ -d "$wrksrc/$build_wrksrc" ] && [ ! -L "$wrksrc/$build_wrksrc" ]; then
	_shared="$XBPS_BUILDDIR/.shared-src/$build_wrksrc"
	rm -rf "$_shared"
	mkdir -p "${_shared%/*}"
	mv "$wrksrc/$build_wrksrc" "$_shared"
	ln -s "$_shared" "$wrksrc/$build_wrksrc"
	unset _shared
fi
