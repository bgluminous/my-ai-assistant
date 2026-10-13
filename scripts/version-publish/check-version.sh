#!/bin/sh
# Local, offline release gate. Run from the project root or supply --root DIR.
set +x
set -eu
set -f
LC_ALL=C; export LC_ALL
umask 077
fail() { printf '%s\n' "$2" >&2; exit "$1"; }
need() { command -v "$1" >/dev/null 2>&1 || fail 2 "E_DEPENDENCY_$1"; }
for tool in sh awk dirname; do need "$tool"; done
dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd) || fail 2 E_SCRIPT_DIR
root=.; branch=; skip_missing=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --root|--branch)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail 2 E_ARGUMENT
            if [ "$1" = --root ]; then root=$2; else branch=$2; fi
            shift 2 ;;
        --skip-missing-settings) skip_missing=true; shift ;;
        *) fail 2 E_ARGUMENT ;;
    esac
done
[ -n "$branch" ] || fail 2 E_RELEASE_BRANCH_REQUIRED
expected=$(sh "$dir/json-tool.sh" ref "$branch") || fail 2 E_RELEASE_BRANCH
CDPATH= cd -P "$root" || fail 2 E_PROJECT_ROOT
if [ ! -e .version-settings ] && [ ! -L .version-settings ]; then
    if [ "$skip_missing" != true ]; then
        printf '%s\n' 'Missing project-root .version-settings. Skip this local check? [y/N]' >&2
        if [ ! -t 0 ]; then
            fail 3 E_SETTINGS_MISSING_CONFIRMATION_REQUIRED
        fi
        answer=
        if IFS= read -r answer; then :; else fail 3 E_SETTINGS_MISSING_NOT_SKIPPED; fi
        case "$answer" in y|Y|yes|YES) skip_missing=true ;; *) fail 3 E_SETTINGS_MISSING_NOT_SKIPPED ;; esac
    fi
    printf 'skipped local version check: missing .version-settings; branch=%s\n' "$branch"
    exit 0
fi
# A skip confirmation only applies to absence, never to a malformed configuration.
[ -f .version-settings ] && [ -r .version-settings ] || fail 2 E_SETTINGS_UNREADABLE
for tool in wc mktemp rm; do need "$tool"; done
[ "$(wc -c < .version-settings)" -le 65536 ] || fail 2 E_SETTINGS_SIZE
tmp=$(mktemp -d "${TMPDIR:-/tmp}/version-check.XXXXXX") || fail 2 E_TEMP
trap 'rm -rf "$tmp"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
awk -f "$dir/settings.awk" ./.version-settings > "$tmp/settings.env" || fail 2 E_SETTINGS_INVALID
while IFS= read -r setting; do
    export "$setting"
done < "$tmp/settings.env"
unset XIL_PUBLISH_KEY GITHUB_REGISTRY_TOKEN GITHUB_RELEASE_TOKEN GITHUB_TOKEN
case "$RELEASE_BACKEND" in xil|github) ;; *) fail 2 E_BACKEND ;; esac
[ -n "$RELEASE_PROJECT_CODE" ] && [ "$RELEASE_PROJECT_CODE" != CHANGE_ME ] || fail 2 E_PROJECT_CONFIGURATION_REQUIRED
hook() {
    case "$1" in ''|-*) fail 2 E_HOOK_PATH ;; esac
    [ -f "$1" ] && [ -r "$1" ] || fail 2 E_HOOK_REQUIRED
    sh -n "$1" > "$tmp/hook.stdout" 2> "$tmp/hook.stderr" || fail 2 E_HOOK_SYNTAX
}
json_check() {
    VP_OP=$1 VP_VALUE=$2 awk -f "$dir/json.awk" -f "$dir/release.awk" > /dev/null || fail 2 E_SETTINGS_VALUE
}
version_hook=${RELEASE_VERSION_HOOK:-ci/read-version.sh}
hook "$version_hook"
raw_version=$(sh "$version_hook" 2> "$tmp/version.stderr") || fail 2 E_VERSION_HOOK
version=$(sh "$dir/json-tool.sh" version "$raw_version") || fail 2 E_VERSION
[ "$version" = "$expected" ] || fail 2 E_BRANCH_VERSION_MISMATCH
mode=${RELEASE_ASSET_MODE:-upload}
case "$mode" in upload|existing) ;; *) fail 2 E_ASSET_MODE ;; esac
asset_name=${RELEASE_ASSET_FILE:-app.tar.gz}
case "$asset_name" in ''|.*|*.|*[!A-Za-z0-9._-]*) fail 2 E_ASSET_NAME ;; esac
if [ "$mode" = upload ]; then hook "${RELEASE_BUILD_HOOK:-ci/build-release.sh}"; fi
timeout=${RELEASE_UPLOAD_TIMEOUT:-900}
case "$timeout" in ''|*[!0-9]*) fail 2 E_UPLOAD_TIMEOUT ;; esac
[ "$timeout" -ge 1 ] && [ "$timeout" -le 86400 ] || fail 2 E_UPLOAD_TIMEOUT
external=${GITHUB_ALLOW_EXTERNAL_URL:-false}
case "$external" in true|false) ;; *) fail 2 E_EXTERNAL_URL_OPTION ;; esac
if [ "$RELEASE_BACKEND" = xil ]; then
    [ "$mode" = upload ] || fail 2 E_XIL_UPLOAD_HOOK_REQUIRED
    hook "${RELEASE_UPLOAD_HOOK:-ci/upload-release.sh}"
    VP_SERVICE=$XIL_SERVICE_URL awk 'BEGIN {
        s=ENVIRON["VP_SERVICE"]
        if (s !~ /^https?:\/\/[^/?#@[:space:]]+\/?$/ || s ~ /[\\"]/ ) exit 2
    }' || fail 2 E_XIL_ORIGIN
else
    json_check repository "${GITHUB_REGISTRY_REPOSITORY:-bgluminous/open-source-registry}"
    json_check git-tag "${GITHUB_REGISTRY_BRANCH:-main}"
    if [ "$external" = true ]; then
        [ "$mode" = existing ] && [ -n "$RELEASE_DOWNLOAD_URL" ] || fail 2 E_EXTERNAL_URL_INPUT
    else
        [ -n "$GITHUB_RELEASE_REPOSITORY" ] || fail 2 E_RELEASE_REPOSITORY_REQUIRED
        json_check repository "$GITHUB_RELEASE_REPOSITORY"
        if [ -n "$GITHUB_RELEASE_TARGET" ]; then
            case "$GITHUB_RELEASE_TARGET" in *[!0-9a-fA-F]*) fail 2 E_RELEASE_TARGET_SHA ;; esac
            [ "${#GITHUB_RELEASE_TARGET}" -eq 40 ] || fail 2 E_RELEASE_TARGET_SHA
        fi
    fi
fi
if [ -n "$RELEASE_PLATFORMS" ]; then
    [ -z "$RELEASE_PLATFORM_CODE" ] || fail 2 E_PLATFORM_CONFIGURATION
    platforms=$RELEASE_PLATFORMS
else platforms=$RELEASE_PLATFORM_CODE; fi
[ -n "$platforms" ] || fail 2 E_PLATFORM
# Project-specific platform mapping keeps the existing shared v<version> Release.
hook ci/release-platform.sh
. ./ci/release-platform.sh
count=0; seen=' '
for platform in $platforms; do
    RELEASE_PLATFORM_CODE=$platform; export RELEASE_PLATFORM_CODE
    maa_release_platform "$version" "$platform"
    [ -n "$GITHUB_RELEASE_ASSET_NAME" ] || fail 2 E_ASSET_SELECTOR
    case "$GITHUB_RELEASE_ASSET_NAME" in *[!A-Za-z0-9._-]*|.*|*.) fail 2 E_ASSET_NAME ;; esac
    sh "$dir/json-tool.sh" make "$version" > "$tmp/release.json" || fail 2 E_RELEASE_INPUT
    case "$seen" in *" $platform "*) fail 2 E_PLATFORM_DUPLICATE ;; esac
    seen="$seen$platform "
    if [ "$RELEASE_BACKEND" = github ] && [ "$external" = false ]; then
        json_check git-tag "${GITHUB_RELEASE_TAG:-$RELEASE_PROJECT_CODE/$platform/v$version}"
    fi
    sh "$dir/publish-version.sh" validate --backend "$RELEASE_BACKEND" --input "$tmp/release.json" \
        --tag "$expected" > "$tmp/validate.stdout" 2> "$tmp/validate.stderr" || fail 2 E_RELEASE_INPUT
    count=$((count + 1))
done
[ "$count" -gt 0 ] || fail 2 E_PLATFORM
printf 'validated local settings: project=%s branch=%s version=%s platforms=%s\n' \
    "$RELEASE_PROJECT_CODE" "$branch" "$version" "$count"
