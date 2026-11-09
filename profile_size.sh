OUT=target/wasm32-unknown-unknown/release/

cargo build --release --features size_test --target wasm32-unknown-unknown
bin_size1=$(stat --format=%s $OUT/nanalloc.wasm)
echo "The current library size is $bin_size1 bytes."

wasm-opt -Oz $OUT/nanalloc.wasm --enable-bulk-memory -o $OUT/profile.nanalloc.wasm
bin_size2=$(stat --format=%s $OUT/profile.nanalloc.wasm)

echo "$bin_size2 when optimized with binaryen."