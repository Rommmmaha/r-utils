DEST_DIR="$HOME/.local/bin"
cargo build --release --workspace
mkdir -p "$DEST_DIR"
BINARY_NAMES=$(cargo metadata --format-version 1 --no-deps | jq -r '.packages[].targets[] | select(.kind[] | contains("bin")) | .name')
for bin in $BINARY_NAMES; do
  if [ -f "target/release/$bin" ]; then
    rm -f "$DEST_DIR/$bin"
    cp "target/release/$bin" "$DEST_DIR/"
  fi
done