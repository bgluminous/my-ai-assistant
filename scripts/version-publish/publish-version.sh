#!/bin/sh
# Copy this directory's .sh and .awk files together. Never enable shell tracing.
set +x
set -eu
set -f
LC_ALL=C; export LC_ALL
fail() { printf '%s\n' "$2" >&2; exit "$1"; }
need() { command -v "$1" >/dev/null 2>&1 || fail 2 "E_DEPENDENCY_$1"; }
for tool in awk dirname mktemp rm cat wc; do need "$tool"; done
vp_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd) || fail 2 E_SCRIPT_DIR
operation=${1:-}; [ "$#" -gt 0 ] || fail 2 E_USAGE; shift
case "$operation" in validate|publish) ;; *) fail 2 E_OPERATION ;; esac
backend= input= asset= tag=
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail 2 E_ARGUMENT
    case "$1" in
        --backend) [ -z "$backend" ] || fail 2 E_ARGUMENT; backend=$2 ;;
        --input) [ -z "$input" ] || fail 2 E_ARGUMENT; input=$2 ;;
        --asset) [ -z "$asset" ] || fail 2 E_ARGUMENT; asset=$2 ;;
        --tag) [ -z "$tag" ] || fail 2 E_ARGUMENT; tag=$2 ;;
        *) fail 2 E_ARGUMENT ;;
    esac
    shift 2
done
case "$backend" in xil|github) ;; *) fail 2 E_BACKEND ;; esac
[ -n "$input" ] && [ -f "$input" ] && [ -r "$input" ] || fail 2 E_INPUT
[ "$(wc -c < "$input")" -le 262144 ] || fail 2 E_INPUT_SIZE
if [ -n "$asset" ]; then
    [ "$backend" = github ] || fail 2 E_ASSET_BACKEND
    [ -f "$asset" ] && [ -r "$asset" ] && [ -s "$asset" ] || fail 2 E_ASSET
fi
umask 077
vp_tmp=$(mktemp -d "${TMPDIR:-/tmp}/version-publish.XXXXXX") || fail 2 E_TEMP
trap 'rm -rf "$vp_tmp"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
json() {
    VP_OP=$1 VP_FILE=${2:-} VP_FILE2=${3:-} \
        awk -f "$vp_dir/json.awk" -f "$vp_dir/release.awk"
}
get() { VP_FIELD=$2 json get "$1"; }
encode_uri() { VP_VALUE=$1 json uri; }
VP_TAG=$tag json validate "$input" > "$vp_tmp/request.json" || fail 2 E_VALIDATION
version=$(get "$vp_tmp/request.json" version) || fail 2 E_VERSION
project=${RELEASE_PROJECT_CODE:-}; platform=${RELEASE_PLATFORM_CODE:-}
if [ "$operation" = validate ]; then
    printf 'validated project=%s platform=%s version=%s\n' "$project" "$platform" "$version"
    exit 0
fi
for tool in curl sleep; do need "$tool"; done
. "$vp_dir/http.sh"
upload_timeout=${RELEASE_UPLOAD_TIMEOUT:-900}
case "$upload_timeout" in ''|*[!0-9]*) fail 2 E_UPLOAD_TIMEOUT ;; esac
[ "$upload_timeout" -ge 1 ] && [ "$upload_timeout" -le 86400 ] || fail 2 E_UPLOAD_TIMEOUT
printf 'null\n' > "$vp_tmp/null.json"
if [ "$backend" = xil ]; then
    [ -n "${XIL_PUBLISH_KEY:-}" ] || fail 2 E_XIL_KEY
    service=${XIL_SERVICE_URL:-}
    VP_SERVICE=$service awk 'BEGIN {
        s=ENVIRON["VP_SERVICE"]
        if (s !~ /^https?:\/\/[^/?#@[:space:]]+\/?$/ || s ~ /[\\"]/ ) exit 2
    }' || fail 2 E_XIL_ORIGIN
    VP_HEADER=X-Publish-Key VP_SECRET=$XIL_PUBLISH_KEY json curl-config > "$vp_tmp/xil.config" || fail 2 E_CREDENTIAL
    attempt=1
    while :; do
        http POST "${service%/}/ver/$project/$platform" "$vp_tmp/request.json" "$vp_tmp/response.json" "$vp_tmp/xil.config"
        if [ "$http_transport" -eq 0 ]; then
            case "$http_code" in
                200|201)
                    outcome=$(VP_HTTP=$http_code json xil-response "$vp_tmp/response.json" "$vp_tmp/request.json") || fail 3 E_XIL_RESPONSE
                    printf '%s project=%s platform=%s version=%s\n' "$outcome" "$project" "$platform" "$version"
                    exit 0 ;;
            esac
        fi
        if ! retryable || [ "$attempt" -ge 3 ]; then fail 4 "E_XIL_HTTP_${http_code}"; fi
        sleep "$attempt"; attempt=$((attempt + 1))
    done
fi
. "$vp_dir/github.sh"
github_publish
