#!/usr/bin/env bash
# Build the playground's generated pieces: the wasm bindings (web/pkg/)
# and the corpus examples (web/examples/). Everything else in web/ is
# static and checked in (vendor/ is pinned, see vendor/LICENSE-*).
set -euo pipefail
cd "$(dirname "$0")/.."

wasm-pack build leadsheet-wasm --target web --release --out-dir ../web/pkg

mkdir -p web/examples
cp corpus/*.ls web/examples/
cp songs/*.ls web/examples/

echo
echo "ready — serve with:  python3 -m http.server -d web 8000"
