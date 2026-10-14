#!/bin/sh
# Read the reviewed, version-specific notes; never infer release text from Git.
set -eu
fail() { printf '%s\n' "$1" >&2; exit 2; }
[ "$#" -eq 1 ] || fail E_CHANGELOG_VERSION
root=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(sh scripts/version-publish/json-tool.sh version "$1") || fail E_CHANGELOG_VERSION
[ "$version" = "$1" ] || fail E_CHANGELOG_VERSION
file=docs/releases/$version.md
[ -f "$file" ] && [ -r "$file" ] || fail E_CHANGELOG_FILE
[ "$(wc -c < "$file")" -le 64000 ] || fail E_CHANGELOG_SIZE
# A heading alone or a placeholder is not a reviewed release note.
# BINMODE keeps Windows awk from hiding CR bytes before validation.
awk -v BINMODE=3 '
    index($0, "\r") { invalid = 1 }
    /^[[:space:]]*[-*][[:space:]]+[^[:space:]]/ { entries++ }
    tolower($0) ~ /^[[:space:]]*[-*][[:space:]]+(todo|tbd|待补充|待填写)[[:space:]。.!！]*$/ { invalid = 1 }
    END { if (invalid || !entries) exit 2 }
' "$file" || fail E_CHANGELOG_CONTENT
cat "$file"
