#!/usr/bin/env bash
# Regenerates the <!-- help:start --> … <!-- help:end --> block in README.md
# from the live `ios --help` output.
#
# Usage: ./scripts/update-readme-help.sh
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IOS="$ROOT/target/release/ios"
README="$ROOT/README.md"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

# Build release binary
cargo build --release --bin ios --manifest-path "$ROOT/Cargo.toml" --features cli

filter_help() {
  python3 -c '
import sys, re

lines = sys.stdin.read().splitlines()
result = []
i = 0
while i < len(lines):
    line = lines[i]
    if re.match(r"^[ ]{4,}--(legacy|udid)\b", line):
        indent = len(line) - len(line.lstrip())
        i += 1
        while i < len(lines) and len(lines[i]) - len(lines[i].lstrip()) > indent:
            i += 1
        continue
    if re.match(r"^[ ]+-h,[ ]+--help\b", line):
        indent = len(line) - len(line.lstrip())
        i += 1
        while i < len(lines) and len(lines[i]) - len(lines[i].lstrip()) > indent:
            i += 1
        continue
    result.append(line)
    i += 1

cleaned = []
i = 0
while i < len(result):
    if result[i].rstrip() == "Options:":
        j = i + 1
        while j < len(result) and result[j].strip() == "":
            j += 1
        if j >= len(result) or result[j][0] != " ":
            while cleaned and cleaned[-1].strip() == "":
                cleaned.pop()
            i = j
            continue
    cleaned.append(result[i])
    i += 1

print("\n".join(cleaned))
'
}

block() {
  local header="$1"; shift
  printf '### `%s`\n\n```\n' "$header"
  "$@" --help 2>&1 | filter_help
  printf '```\n\n'
}

{
  block "ios"          "$IOS"
  block "ios devices"  "$IOS" devices
  block "ios info"     "$IOS" info
  block "ios services" "$IOS" services
  block "ios relay"    "$IOS" relay
  block "ios watch"    "$IOS" watch
  block "ios version"  "$IOS" version
  block "ios apps"     "$IOS" apps
  block "ios orientation" "$IOS" orientation
  block "ios lang"     "$IOS" lang
  block "ios date"     "$IOS" date
  block "ios rsd"      "$IOS" rsd
  block "ios mounter"  "$IOS" mounter
  block "ios perf"     "$IOS" perf
  block "ios runtest"  "$IOS" runtest
  block "ios runwda"   "$IOS" runwda
} > "$TMP"

python3 - "$README" "$TMP" <<'EOF'
import sys, pathlib

readme_path, help_path = sys.argv[1], sys.argv[2]
readme = pathlib.Path(readme_path).read_text()
help_text = pathlib.Path(help_path).read_text()

start_marker = "<!-- help:start -->"
end_marker   = "<!-- help:end -->"

start = readme.index(start_marker) + len(start_marker)
end   = readme.index(end_marker)

pathlib.Path(readme_path).write_text(
    readme[:start] + "\n" + help_text + readme[end:]
)
print("README.md updated.")
EOF
