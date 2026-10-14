#!/bin/sh
# Prepare offline, or register a published Release using the saved input.
set +x
set -eu
set -f
umask 077
fail() { printf '%s\n' "$2" >&2; exit "$1"; }
phase=${1:-}; branch=${2:-}; detail=${3:-}
[ "$#" -eq 3 ] || fail 2 E_ARGUMENT
case "$phase" in prepare|publish) ;; *) fail 2 E_PHASE ;; esac
root=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
cd "$root"
publisher=scripts/version-publish
sh "$publisher/check-version.sh" --branch "$branch"

# Use the same data parser as the local gate. Do not source .version-settings.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/maa-release.XXXXXX") || fail 2 E_TEMP
trap 'rm -rf "$tmp"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
awk -f "$publisher/settings.awk" .version-settings > "$tmp/settings.env" || fail 2 E_SETTINGS_INVALID
while IFS= read -r setting; do export "$setting"; done < "$tmp/settings.env"
version=$(sh "$RELEASE_VERSION_HOOK") || fail 2 E_VERSION_HOOK
. ./ci/release-platform.sh

if [ "$phase" = prepare ]; then
    [ -f "$detail" ] && [ -r "$detail" ] || fail 2 E_CHANGELOG_FILE
    sh ci/read-release-notes.sh "$version" > "$tmp/changelog.md" || fail 2 E_CHANGELOG
    cmp -s "$detail" "$tmp/changelog.md" || fail 2 E_CHANGELOG_MISMATCH
    RELEASE_CHANGELOG_FILE=$detail; export RELEASE_CHANGELOG_FILE
    for platform in $RELEASE_PLATFORMS; do
        RELEASE_PLATFORM_CODE=$platform; export RELEASE_PLATFORM_CODE
        maa_release_platform "$version" "$platform"
        mkdir -p "release-out/$platform"
        printf '{"updater":{"manifestUrl":"https://github.com/%s/releases/download/v%s/updater-%s.json"}}\n' \
            "$GITHUB_RELEASE_REPOSITORY" "$version" "$platform" > "$tmp/extra.json"
        RELEASE_EXTRA_FILE=$tmp/extra.json; export RELEASE_EXTRA_FILE
        sh "$publisher/json-tool.sh" make "$version" > "$tmp/release.json"
        sh "$publisher/publish-version.sh" validate --backend github --input "$tmp/release.json" --tag "$version"
        # Saved once by prepare; failed registration jobs reuse this exact input.
        cp "$tmp/release.json" "release-out/$platform/release.json"
    done
    exit 0
fi

case " $RELEASE_PLATFORMS " in *" $detail "*) ;; *) fail 2 E_PROJECT_PLATFORM ;; esac
RELEASE_PLATFORM_CODE=$detail; export RELEASE_PLATFORM_CODE
maa_release_platform "$version" "$detail"
input=release-out/$detail/release.json
[ -f "$input" ] && [ -r "$input" ] || fail 2 E_PREPARED_INPUT_REQUIRED
saved_version=$(sh "$publisher/json-tool.sh" get "$input" version) || fail 2 E_PREPARED_INPUT
[ "$saved_version" = "$version" ] || fail 2 E_PREPARED_VERSION_MISMATCH
saved_manifest=$(sh "$publisher/json-tool.sh" get "$input" extra.updater.manifestUrl) || fail 2 E_PREPARED_MANIFEST
[ "$saved_manifest" = "https://github.com/$GITHUB_RELEASE_REPOSITORY/releases/download/v$version/updater-$detail.json" ] || fail 2 E_PREPARED_MANIFEST
# No --asset: this path verifies an existing public Release and never uploads.
sh "$publisher/publish-version.sh" publish --backend github --input "$input" --tag "$version"
