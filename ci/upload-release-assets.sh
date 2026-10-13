#!/bin/sh
# Upload to the shared draft without replacing any previously uploaded bytes.
# This project helper uses gh's built-in JSON queries and the runner's sha256sum.
set +x
set -eu
umask 077
fail() { printf '%s\n' "$1" >&2; exit 2; }
[ -n "${GH_REPO:-}" ] && [ -n "${TAG:-}" ] || fail E_RELEASE_CONFIGURATION
[ "$#" -gt 0 ] || fail E_ASSET_REQUIRED
tmp=$(mktemp -d "${TMPDIR:-/tmp}/maa-assets.XXXXXX") || fail E_TEMP
trap 'rm -rf "$tmp"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
gh api "repos/$GH_REPO/releases/tags/$TAG" --jq '[.id, .draft] | @tsv' > "$tmp/release.tsv"
IFS="$(printf ' \t\r')" read -r release_id draft < "$tmp/release.tsv"
[ "$draft" = true ] || fail E_RELEASE_ALREADY_PUBLISHED
case "$release_id" in ''|*[!0-9]*) fail E_RELEASE_ID ;; esac
list_assets() {
    gh api --paginate "repos/$GH_REPO/releases/$release_id/assets?per_page=100" \
        --jq '.[] | [.name, .state, .size, .digest] | @tsv' > "$tmp/assets.tsv"
}
asset_count() { VP_NAME=$name awk -F '\t' '$1 == ENVIRON["VP_NAME"] { count++ } END { print count + 0 }' "$tmp/assets.tsv"; }
list_assets
for asset in "$@"; do
    [ -f "$asset" ] && [ -s "$asset" ] || fail E_ASSET_REQUIRED
    name=${asset##*/}
    case "$name" in ''|.*|*.|*[!A-Za-z0-9._-]*) fail E_ASSET_NAME ;; esac
    count=$(asset_count)
    if [ "$count" = 0 ]; then
        # Without --clobber, a concurrent same-name upload fails closed.
        gh release upload "$TAG" "$asset"
        list_assets
        count=$(asset_count)
    fi
    [ "$count" = 1 ] || fail E_ASSET_LOOKUP
    size=$(wc -c < "$asset"); size=$((size + 0))
    sha256sum < "$asset" > "$tmp/digest"
    read -r digest rest < "$tmp/digest"
    VP_NAME=$name VP_SIZE=$size VP_DIGEST=sha256:$digest awk -F '\t' '
        { sub(/\r$/, "") }
        $1 == ENVIRON["VP_NAME"] {
            if ($2 != "uploaded") { print "E_ASSET_INCOMPLETE" > "/dev/stderr"; exit 2 }
            if ($3 != ENVIRON["VP_SIZE"]) { print "E_ASSET_SIZE_CONFLICT" > "/dev/stderr"; exit 2 }
            if ($4 != ENVIRON["VP_DIGEST"]) { print "E_ASSET_DIGEST_CONFLICT" > "/dev/stderr"; exit 2 }
        }
    ' "$tmp/assets.tsv"
    printf 'verified release asset: %s\n' "$name"
done
