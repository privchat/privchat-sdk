#!/bin/bash
# Generate UniFFI bindings for all supported languages

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
UDL_FILE="$PROJECT_DIR/src/api.udl"
BINDINGS_DIR="$PROJECT_DIR/bindings"

echo "🔨 Generating UniFFI bindings for Privchat SDK"
echo "UDL file: $UDL_FILE"
echo "Output directory: $BINDINGS_DIR"
echo ""

# Create bindings directory
mkdir -p "$BINDINGS_DIR"

# Generate Kotlin bindings
echo "📱 Generating Kotlin bindings..."
cargo run --bin uniffi-bindgen generate \
    "$UDL_FILE" \
    --language kotlin \
    --out-dir "$BINDINGS_DIR/kotlin"
echo "✅ Kotlin bindings generated"
echo ""

# Generate Swift bindings
echo "🍎 Generating Swift bindings..."
cargo run --bin uniffi-bindgen generate \
    "$UDL_FILE" \
    --language swift \
    --out-dir "$BINDINGS_DIR/swift"
echo "✅ Swift bindings generated"
echo ""

# Generate Python bindings
echo "🐍 Generating Python bindings..."
cargo run --bin uniffi-bindgen generate \
    "$UDL_FILE" \
    --language python \
    --out-dir "$BINDINGS_DIR/python"
echo "✅ Python bindings generated"
echo ""

# Generate Ruby bindings
echo "💎 Generating Ruby bindings..."
cargo run --bin uniffi-bindgen generate \
    "$UDL_FILE" \
    --language ruby \
    --out-dir "$BINDINGS_DIR/ruby"
echo "✅ Ruby bindings generated"
echo ""

echo "🎉 All bindings generated successfully!"
echo ""
echo "Bindings are located in:"
echo "  - Kotlin: $BINDINGS_DIR/kotlin/"
echo "  - Swift:  $BINDINGS_DIR/swift/"
echo "  - Python: $BINDINGS_DIR/python/"
echo "  - Ruby:   $BINDINGS_DIR/ruby/"
