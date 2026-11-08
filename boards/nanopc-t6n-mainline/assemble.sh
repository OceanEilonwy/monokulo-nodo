# Sourced by the image builder. Runs the vendor board's own assembly unchanged, so the partition
# layout and the bootloader files are byte for byte the same, then names the result for this board.
# $out is this board's directory, so the vendor image is never touched.
. /src/boards/nanopc-t6n/assemble.sh
rm -f "$img.sha256"
mv "$img" "$out/monokulo-nanopc-t6n-mainline-sd.img"
img=$out/monokulo-nanopc-t6n-mainline-sd.img
(cd "$out" && sha256sum "$(basename "$img")") | tee "$img.sha256"
