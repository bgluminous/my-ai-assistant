#!/bin/sh
# Small data-only helper for CI inputs; never eval/source JSON or its output.
set -eu
LC_ALL=C; export LC_ALL
for tool in dirname awk; do
    command -v "$tool" >/dev/null 2>&1 || { printf 'E_DEPENDENCY_%s\n' "$tool" >&2; exit 2; }
done
dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd) || exit 2
VP_OP=${1:-}; export VP_OP
case "$VP_OP" in
    get) [ "$#" -eq 3 ] || exit 2; VP_FILE=$2; VP_FIELD=$3; export VP_FILE VP_FIELD ;;
    make) [ "$#" -eq 2 ] || exit 2; RELEASE_VERSION=$2; export RELEASE_VERSION ;;
    version|ref) [ "$#" -eq 2 ] || exit 2; VP_VALUE=$2; export VP_VALUE ;;
    canonical) [ "$#" -eq 2 ] || exit 2; VP_FILE=$2; export VP_FILE ;;
    *) printf '%s\n' 'Usage: json-tool.sh get FILE FIELD | make VERSION | version VERSION | ref BRANCH | canonical FILE' >&2; exit 2 ;;
esac
exec awk -f "$dir/json.awk" -f "$dir/release.awk"
