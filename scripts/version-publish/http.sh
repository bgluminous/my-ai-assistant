#!/bin/sh
# Sourced only by publish-version.sh. curl 7.29-compatible flags; no redirects.
http() {
    http_method=$1; http_url=$2; http_body=$3; http_output=$4; http_config=$5
    http_kind=${6:-json}; http_accept=${7:-application/vnd.github+json}
    set -- curl -q --config "$http_config" --request "$http_method" \
        --url "$http_url" --output "$http_output" --write-out '%{http_code}' \
        --header "Accept: $http_accept"
    if [ "$http_kind" = asset ]; then
        set -- "$@" --max-time "$upload_timeout" --header 'Content-Type: application/octet-stream' \
            --upload-file "$http_body"
    else
        set -- "$@" --max-time 30 --header 'Content-Type: application/json'
        if [ -n "$http_body" ]; then set -- "$@" --data-binary "@$http_body"; fi
    fi
    http_transport=0
    if http_code=$("$@" 2> "$vp_tmp/curl.error"); then :; else http_transport=$?; fi
    case "$http_code" in [0-9][0-9][0-9]) ;; *) http_code=000 ;; esac
}
retryable() {
    if [ "$http_transport" -ne 0 ]; then
        case "$http_transport" in 5|6|7|18|28|52|55|56) return 0 ;; *) return 1 ;; esac
    fi
    case "$http_code" in 408|429|500|502|503|504) return 0 ;; *) return 1 ;; esac
}
api() {
    api_attempt=1
    while :; do
        http "$@"
        if [ "$http_transport" -eq 0 ] && ! retryable; then return 0; fi
        if ! retryable || [ "$api_attempt" -ge 3 ]; then fail 3 "E_HTTP_${http_code}"; fi
        sleep "$api_attempt"; api_attempt=$((api_attempt + 1))
    done
}
expect_http() {
    [ "$http_transport" -eq 0 ] || fail 3 E_TRANSPORT
    [ "$http_code" = "$1" ] || fail 4 "E_HTTP_${http_code}"
}
