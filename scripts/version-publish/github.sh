#!/bin/sh
# GitHub registry and project Releases have separate repositories and credentials.
gh_scalar() { get "$1" "$2" || fail 3 E_GITHUB_RESPONSE; }
gh_same_repository() {
    VP_REPO_LEFT=$1 VP_REPO_RIGHT=$2 awk 'BEGIN {
        exit(tolower(ENVIRON["VP_REPO_LEFT"]) != tolower(ENVIRON["VP_REPO_RIGHT"]))
    }'
}
gh_id() {
    gh_number=$(gh_scalar "$1" id)
    case "$gh_number" in ''|*[!0-9]*) fail 3 E_GITHUB_ID ;; esac
    printf '%s\n' "$gh_number"
}
gh_sha() {
    gh_sha_value=$(gh_scalar "$1" "$2")
    case "$gh_sha_value" in ''|*[!0-9a-f]*) fail 3 E_GITHUB_SHA ;; esac
    [ "${#gh_sha_value}" -eq 40 ] || fail 3 E_GITHUB_SHA
    printf '%s\n' "$gh_sha_value"
}
gh_collect() {
    collect_url=$1; collect_output=$2; collect_config=$3; collect_page=1
    printf '[]\n' > "$collect_output"
    while :; do
        api GET "$collect_url?per_page=100&page=$collect_page" '' "$vp_tmp/page.json" "$collect_config"
        expect_http 200
        collect_count=$(json count "$vp_tmp/page.json") || fail 3 E_GITHUB_PAGE
        json array-join "$collect_output" "$vp_tmp/page.json" > "$vp_tmp/join.json" || fail 3 E_GITHUB_PAGE
        cat "$vp_tmp/join.json" > "$collect_output"
        [ "$collect_count" -eq 100 ] || break
        collect_page=$((collect_page + 1))
        [ "$collect_page" -le 1000 ] || fail 3 E_PAGINATION_LIMIT
    done
}
gh_snapshot() {
    api GET "$registry_api/git/ref/heads/$registry_branch_uri" '' "$vp_tmp/ref.json" "$vp_tmp/registry.config"
    case "$http_code" in 404|409) fail 4 E_REGISTRY_BRANCH_INITIALIZATION_REQUIRED ;; esac
    expect_http 200
    registry_head=$(gh_sha "$vp_tmp/ref.json" object.sha)
    api GET "$registry_api/git/commits/$registry_head" '' "$vp_tmp/head.json" "$vp_tmp/registry.config"
    expect_http 200
    registry_tree=$(gh_sha "$vp_tmp/head.json" tree.sha)
    for snapshot_name in old latest index; do
        case "$snapshot_name" in
            old) snapshot_path="$registry_prefix/versions/$version.json" ;;
            *) snapshot_path="$registry_prefix/$snapshot_name.json" ;;
        esac
        api GET "$registry_api/contents/$snapshot_path?ref=$registry_head" '' "$vp_tmp/$snapshot_name.json" \
            "$vp_tmp/registry.config" json application/vnd.github.raw+json
        case "$http_code" in
            200) json canonical "$vp_tmp/$snapshot_name.json" > "$vp_tmp/canonical.json" || fail 3 E_REGISTRY_JSON
                 cat "$vp_tmp/canonical.json" > "$vp_tmp/$snapshot_name.json" ;;
            404) if [ "$snapshot_name" = index ]; then printf '[]\n'; else printf 'null\n'; fi > "$vp_tmp/$snapshot_name.json" ;;
            *) fail 4 "E_REGISTRY_HTTP_${http_code}" ;;
        esac
    done
}
gh_plan() {
    VP_LATEST=$vp_tmp/latest.json VP_INDEX=$vp_tmp/index.json VP_DIR=$vp_tmp/plan \
        json plan "$1" "$vp_tmp/old.json" || fail 4 E_REGISTRY_CONFLICT
    outcome=$(cat "$vp_tmp/plan/outcome")
}
gh_find_release() {
    api GET "$release_api/releases/tags/$release_tag_uri" '' "$vp_tmp/release.json" "$vp_tmp/release.config"
    if [ "$http_code" = 404 ]; then
        gh_collect "$release_api/releases" "$vp_tmp/releases.json" "$vp_tmp/release.config"
        VP_RELEASE_TAG=$release_tag json pick-release "$vp_tmp/releases.json" > "$vp_tmp/release.json" || fail 3 E_RELEASE_LOOKUP
    else expect_http 200; fi
    release_id=
    if [ "$(cat "$vp_tmp/release.json")" != null ]; then
        release_id=$(gh_id "$vp_tmp/release.json")
        [ "$(gh_scalar "$vp_tmp/release.json" tag_name)" = "$release_tag" ] || fail 3 E_RELEASE_TAG
        [ "$(gh_scalar "$vp_tmp/release.json" prerelease)" = false ] || fail 4 E_PRERELEASE
        release_draft=$(gh_scalar "$vp_tmp/release.json" draft)
        case "$release_draft" in true|false) ;; *) fail 3 E_RELEASE_STATE ;; esac
    fi
}
gh_find_asset() {
    gh_collect "$release_api/releases/$release_id/assets" "$vp_tmp/assets.json" "$vp_tmp/release.config"
    VP_ASSET_NAME=$asset_name VP_VALUE=$supplied_url json pick-asset "$vp_tmp/assets.json" > "$vp_tmp/asset.json" || fail 3 E_ASSET_LOOKUP
    asset_id=
    if [ "$(cat "$vp_tmp/asset.json")" != null ]; then
        asset_id=$(gh_id "$vp_tmp/asset.json")
        [ "$(gh_scalar "$vp_tmp/asset.json" state)" = uploaded ] || fail 4 E_ASSET_INCOMPLETE
        if [ -n "$asset" ]; then
            [ "$(gh_scalar "$vp_tmp/asset.json" digest)" = "sha256:$asset_sha" ] || fail 4 E_ASSET_DIGEST_CONFLICT
            [ "$(gh_scalar "$vp_tmp/asset.json" size)" = "$asset_size" ] || fail 4 E_ASSET_SIZE_CONFLICT
        fi
        download_url=$(gh_scalar "$vp_tmp/asset.json" browser_download_url)
        VP_URL=$download_url VP_REPO=$release_repo awk 'BEGIN {
            p="https://github.com/" ENVIRON["VP_REPO"] "/releases/download/"
            if (index(tolower(ENVIRON["VP_URL"]), tolower(p)) != 1) exit 2
        }' || fail 3 E_ASSET_DOWNLOAD_REPOSITORY
        if [ -n "$supplied_url" ] && [ "$supplied_url" != "$download_url" ]; then fail 4 E_DOWNLOAD_MISMATCH; fi
    fi
}
gh_release() {
    api GET "$release_api" '' "$vp_tmp/repo.json" "$vp_tmp/release.config"
    expect_http 200
    [ "$(gh_scalar "$vp_tmp/repo.json" private)" = false ] || fail 4 E_PUBLIC_DOWNLOAD_REQUIRED
    release_attempt=1
    while :; do
        gh_find_release
        [ -z "$release_id" ] || break
        [ -n "$asset" ] || fail 4 E_RELEASE_NOT_FOUND
        [ "$indexed" = false ] || fail 4 E_INDEXED_RELEASE_MISSING
        [ -n "$release_token" ] || fail 2 E_RELEASE_TOKEN
        release_target=${GITHUB_RELEASE_TARGET:-${GITHUB_SHA:-${CI_COMMIT_SHA:-}}}
        case "$release_target" in ''|*[!0-9a-fA-F]*) fail 2 E_RELEASE_TARGET_SHA ;; esac
        [ "${#release_target}" -eq 40 ] || fail 2 E_RELEASE_TARGET_SHA
        api GET "$release_api/commits/$release_target" '' "$vp_tmp/target.json" "$vp_tmp/release.config"
        expect_http 200
        VP_RELEASE_TAG=$release_tag VP_TARGET=$release_target json release-create "$vp_tmp/request.json" > "$vp_tmp/create-release.json" || fail 2 E_RELEASE_BODY
        http POST "$release_api/releases" "$vp_tmp/create-release.json" "$vp_tmp/create-result.json" "$vp_tmp/release.config"
        # Never blindly repeat a POST: the next iteration looks up its result.
        if [ "$http_transport" -eq 0 ]; then
            case "$http_code" in 201|422) ;; *) retryable || fail 4 "E_RELEASE_HTTP_${http_code}" ;; esac
        else retryable || fail 3 E_RELEASE_TRANSPORT; fi
        gh_find_release
        [ -z "$release_id" ] || break
        [ "$release_attempt" -lt 3 ] || fail 4 E_RELEASE_UNCONFIRMED
        sleep "$release_attempt"; release_attempt=$((release_attempt + 1))
    done
    gh_find_asset
    if [ -z "$asset" ]; then
        [ "$release_draft" = false ] || fail 4 E_RELEASE_DRAFT
        [ -n "$asset_id" ] || fail 4 E_ASSET_NOT_FOUND
        return
    fi
    asset_attempt=1
    while [ -z "$asset_id" ]; do
        [ "$indexed" = false ] || fail 4 E_INDEXED_ASSET_MISSING
        [ -n "$release_token" ] || fail 2 E_RELEASE_TOKEN
        # Only the official upload origin for this exact repository/release may
        # receive its token; do not follow a server-supplied arbitrary URL.
        upload_url=$(gh_scalar "$vp_tmp/release.json" upload_url)
        upload_url=${upload_url%%\{*}
        upload_path=${upload_url#https://uploads.github.com/repos/}
        [ "$upload_path" != "$upload_url" ] || fail 3 E_UPLOAD_ORIGIN
        upload_suffix=/releases/$release_id/assets
        upload_repo=${upload_path%"$upload_suffix"}
        [ "$upload_repo" != "$upload_path" ] && gh_same_repository "$upload_repo" "$release_repo" || fail 3 E_UPLOAD_ORIGIN
        asset_name_uri=$(encode_uri "$asset_name") || fail 2 E_ASSET_NAME
        http POST "$upload_url?name=$asset_name_uri" "$asset" "$vp_tmp/upload.json" "$vp_tmp/release.config" asset
        if [ "$http_transport" -eq 0 ]; then
            case "$http_code" in 201|422) ;; *) retryable || fail 4 "E_UPLOAD_HTTP_${http_code}" ;; esac
        else retryable || fail 3 E_UPLOAD_TRANSPORT; fi
        gh_find_asset
        [ -z "$asset_id" ] || break
        [ "$asset_attempt" -lt 3 ] || fail 4 E_UPLOAD_UNCONFIRMED
        sleep "$asset_attempt"; asset_attempt=$((asset_attempt + 1))
    done
    if [ "$release_draft" = true ]; then
        [ -n "$release_token" ] || fail 2 E_RELEASE_TOKEN
        printf '%s\n' '{"draft":false,"make_latest":"false"}' > "$vp_tmp/publish-release.json"
        api PATCH "$release_api/releases/$release_id" "$vp_tmp/publish-release.json" "$vp_tmp/published.json" "$vp_tmp/release.config"
        expect_http 200
        gh_find_release
        [ -n "$release_id" ] && [ "$release_draft" = false ] || fail 3 E_RELEASE_UNCONFIRMED
    fi
}
github_publish() {
    need mkdir
    registry_repo=${GITHUB_REGISTRY_REPOSITORY:-bgluminous/open-source-registry}
    registry_repo=$(VP_VALUE=$registry_repo json repository) || fail 2 E_REGISTRY_REPOSITORY
    registry_branch=${GITHUB_REGISTRY_BRANCH:-main}
    VP_VALUE=$registry_branch json git-tag > /dev/null || fail 2 E_REGISTRY_BRANCH
    registry_branch_uri=$(encode_uri "$registry_branch") || fail 2 E_REGISTRY_BRANCH
    [ -n "${GITHUB_REGISTRY_TOKEN:-}" ] || fail 2 E_REGISTRY_TOKEN
    VP_HEADER=Authorization VP_SECRET="Bearer $GITHUB_REGISTRY_TOKEN" json curl-config > "$vp_tmp/registry.config" || fail 2 E_CREDENTIAL
    registry_api=https://api.github.com/repos/$registry_repo
    registry_prefix=projects/$project/$platform
    supplied_url=$(get "$vp_tmp/request.json" downloadUrl 2>/dev/null) || supplied_url=
    [ "$supplied_url" != null ] || supplied_url=
    external=${GITHUB_ALLOW_EXTERNAL_URL:-false}
    case "$external" in true|false) ;; *) fail 2 E_EXTERNAL_URL_OPTION ;; esac
    if [ "$external" = true ]; then
        [ -z "$asset" ] && [ -n "$supplied_url" ] || fail 2 E_EXTERNAL_URL_INPUT
    else
        release_repo=${GITHUB_RELEASE_REPOSITORY:-}
        if [ -z "$release_repo" ] && [ "${GITHUB_ACTIONS:-false}" = true ]; then release_repo=${GITHUB_REPOSITORY:-}; fi
        [ -n "$release_repo" ] || fail 2 E_RELEASE_REPOSITORY_REQUIRED
        release_repo=$(VP_VALUE=$release_repo json repository) || fail 2 E_RELEASE_REPOSITORY
        release_api=https://api.github.com/repos/$release_repo
        release_token=${GITHUB_RELEASE_TOKEN:-}
        if [ -z "$release_token" ] && [ "${GITHUB_ACTIONS:-false}" = true ] && gh_same_repository "$release_repo" "${GITHUB_REPOSITORY:-}"; then
            release_token=${GITHUB_TOKEN:-}
        fi
        VP_HEADER=Authorization VP_SECRET="${release_token:+Bearer $release_token}" json curl-config > "$vp_tmp/release.config" || fail 2 E_CREDENTIAL
        release_tag=${GITHUB_RELEASE_TAG:-$project/$platform/v$version}
        VP_VALUE=$release_tag json git-tag > /dev/null || fail 2 E_RELEASE_TAG
        release_tag_uri=$(encode_uri "$release_tag") || fail 2 E_RELEASE_TAG
        asset_name=${GITHUB_RELEASE_ASSET_NAME:-}
        if [ -n "$asset" ]; then
            [ -n "$asset_name" ] || asset_name=${asset##*/}
            need sha256sum
            sha256sum < "$asset" > "$vp_tmp/digest" || fail 2 E_ASSET_HASH
            read -r asset_sha digest_rest < "$vp_tmp/digest" || fail 2 E_ASSET_HASH
            asset_size=$(wc -c < "$asset")
            # Remove padding from wc's portable output without a pipeline.
            asset_size=$((asset_size + 0))
        fi
        if [ -n "$asset_name" ]; then
            case "$asset_name" in *[!A-Za-z0-9._-]*|.*|*.) fail 2 E_ASSET_NAME ;; esac
        elif [ -z "$supplied_url" ]; then fail 2 E_ASSET_SELECTOR; fi
    fi
    mkdir "$vp_tmp/plan" || fail 2 E_TEMP
    gh_snapshot
    indexed=false
    if [ "$(cat "$vp_tmp/old.json")" != null ]; then indexed=true; fi
    # Validate registry consistency/version policy before any release write.
    preflight_url=$supplied_url
    if [ -z "$preflight_url" ] && [ "$indexed" = true ]; then preflight_url=$(gh_scalar "$vp_tmp/old.json" downloadUrl); fi
    [ -n "$preflight_url" ] || preflight_url=https://example.invalid/pending-upload
    VP_VALUE=$preflight_url json url "$vp_tmp/request.json" > "$vp_tmp/preflight.json" || fail 2 E_DOWNLOAD_URL
    gh_plan "$vp_tmp/preflight.json"
    if [ "$external" = true ]; then download_url=$supplied_url; else gh_release; fi
    VP_VALUE=$download_url json url "$vp_tmp/request.json" > "$vp_tmp/ready.json" || fail 2 E_DOWNLOAD_URL
    registry_attempt=1
    while :; do
        gh_snapshot
        gh_plan "$vp_tmp/ready.json"
        if [ "$outcome" = unchanged ]; then break; fi
        VP_BASE_TREE=$registry_tree VP_PREFIX=$registry_prefix VP_VERSION=$version VP_DIR=$vp_tmp/plan \
            json tree "$vp_tmp/ready.json" > "$vp_tmp/tree-request.json" || fail 2 E_TREE_BODY
        api POST "$registry_api/git/trees" "$vp_tmp/tree-request.json" "$vp_tmp/tree-result.json" "$vp_tmp/registry.config"
        expect_http 201
        new_tree=$(gh_sha "$vp_tmp/tree-result.json" sha)
        VP_TREE=$new_tree VP_HEAD=$registry_head VP_VERSION=$version json commit "$vp_tmp/ready.json" > "$vp_tmp/commit-request.json" || fail 2 E_COMMIT_BODY
        api POST "$registry_api/git/commits" "$vp_tmp/commit-request.json" "$vp_tmp/commit-result.json" "$vp_tmp/registry.config"
        expect_http 201
        new_commit=$(gh_sha "$vp_tmp/commit-result.json" sha)
        VP_SHA=$new_commit json update-ref "$vp_tmp/ready.json" > "$vp_tmp/ref-request.json" || fail 2 E_REF_BODY
        http PATCH "$registry_api/git/refs/heads/$registry_branch_uri" "$vp_tmp/ref-request.json" "$vp_tmp/ref-result.json" "$vp_tmp/registry.config"
        if [ "$http_transport" -eq 0 ] && [ "$http_code" = 200 ]; then
            [ "$(gh_sha "$vp_tmp/ref-result.json" object.sha)" = "$new_commit" ] || fail 3 E_REF_RESPONSE
            break
        fi
        if [ "$http_transport" -eq 0 ]; then
            case "$http_code" in 409|422) ;; *) retryable || fail 4 "E_REF_HTTP_${http_code}" ;; esac
        else retryable || fail 3 E_REF_TRANSPORT; fi
        if [ "$registry_attempt" -ge 3 ]; then
            # A lost last response can still have committed successfully.
            gh_snapshot; gh_plan "$vp_tmp/ready.json"
            [ "$outcome" = unchanged ] || fail 4 E_REGISTRY_RETRY_EXHAUSTED
            break
        fi
        sleep "$registry_attempt"; registry_attempt=$((registry_attempt + 1))
    done
    printf '%s project=%s platform=%s version=%s\n' "$outcome" "$project" "$platform" "$version"
}
