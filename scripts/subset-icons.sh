#!/bin/bash
# Rebuilds assets/Phosphor-Fill.ttf with only the icons MacSweep uses
# (the `\u{E…}` constants in src/theme.rs), from the full font in
# assets/source/. Run it after adding an icon; `cargo test` fails if an
# icon in theme.rs is missing from the subset.
# Needs fontTools: pip install fonttools
set -euo pipefail
cd "$(dirname "$0")/.."
codes=$(grep -o '\\u{E[0-9A-Fa-f]\{3\}}' src/theme.rs | tr -d '\\u{}' | sort -u | paste -sd, -)
python3 -m fontTools.subset assets/source/Phosphor-Fill.ttf \
    --unicodes="$codes" --no-hinting --desubroutinize \
    --layout-features='' --name-IDs='*' --name-legacy --name-languages='*' \
    --output-file=assets/Phosphor-Fill.ttf
echo "assets/Phosphor-Fill.ttf: $(wc -c < assets/Phosphor-Fill.ttf) bytes, $(echo "$codes" | tr ',' '\n' | wc -l | tr -d ' ') icons"
