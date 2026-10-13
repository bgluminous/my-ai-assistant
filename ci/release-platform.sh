#!/bin/sh
# Trusted project adapter, sourced after .version-settings has been parsed.
# The common publisher still receives the project's actual Release-* branch.
[ "$RELEASE_BACKEND" = github ] || fail 2 E_PROJECT_BACKEND
[ "$RELEASE_PROJECT_CODE" = my-ai-assistant ] || fail 2 E_PROJECT_CODE
[ "$RELEASE_ASSET_MODE" = existing ] || fail 2 E_PROJECT_ASSET_MODE
[ "$RELEASE_PLATFORMS" = 'windows-x64 macos-arm64' ] && [ -z "$RELEASE_PLATFORM_CODE" ] || fail 2 E_PROJECT_PLATFORMS
[ "${GITHUB_ALLOW_EXTERNAL_URL:-false}" = false ] || fail 2 E_PROJECT_EXTERNAL_URL
[ -z "$GITHUB_RELEASE_TAG$GITHUB_RELEASE_ASSET_NAME$RELEASE_DOWNLOAD_URL" ] || fail 2 E_DERIVED_ASSET_CONFIGURATION

maa_release_platform() {
    GITHUB_RELEASE_TAG=v$1
    case "$2" in
        windows-x64) GITHUB_RELEASE_ASSET_NAME=my-ai-assistant_${1}_x64-setup.exe ;;
        macos-arm64) GITHUB_RELEASE_ASSET_NAME=my-ai-assistant_${1}_aarch64.dmg ;;
        *) fail 2 E_PROJECT_PLATFORM ;;
    esac
    export GITHUB_RELEASE_TAG GITHUB_RELEASE_ASSET_NAME
}
