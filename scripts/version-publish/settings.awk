# Parse .version-settings as data. Never emit executable shell statements.
function settings_fail(code) {
    bad = 1
    print code " line=" NR > "/dev/stderr"
    exit 2
}
function strip(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
function setting_value(s,    first, i, tail) {
    s = strip(s); first = substr(s, 1, 1)
    if (first == "\"" || first == sprintf("%c", 39)) {
        for (i = 2; i <= length(s); i++) {
            if (substr(s, i, 1) != first) continue
            tail = strip(substr(s, i + 1))
            if (tail == "" || substr(tail, 1, 1) == "#") return substr(s, 2, i - 2)
            settings_fail("E_SETTINGS_QUOTE")
        }
        settings_fail("E_SETTINGS_QUOTE")
    }
    if (substr(s, 1, 1) == "#") return ""
    if (match(s, /[ \t]+#/)) s = substr(s, 1, RSTART - 1)
    return strip(s)
}
BEGIN {
    keys = "RELEASE_BACKEND RELEASE_PROJECT_CODE RELEASE_PLATFORM_CODE RELEASE_PLATFORMS " \
        "RELEASE_VERSION_HOOK RELEASE_BUILD_HOOK RELEASE_UPLOAD_HOOK RELEASE_ASSET_FILE RELEASE_ASSET_MODE " \
        "RELEASE_DOWNLOAD_URL RELEASE_CHANGELOG_FILE RELEASE_EXTRA_FILE RELEASE_FORCE_UPDATE RELEASE_UPLOAD_TIMEOUT " \
        "XIL_SERVICE_URL GITHUB_REGISTRY_REPOSITORY GITHUB_REGISTRY_BRANCH GITHUB_RELEASE_REPOSITORY " \
        "GITHUB_RELEASE_TAG GITHUB_RELEASE_ASSET_NAME GITHUB_RELEASE_TARGET GITHUB_ALLOW_EXTERNAL_URL"
    count = split(keys, names, " ")
    for (i = 1; i <= count; i++) allowed[names[i]] = 1
}
{
    line = $0; sub(/\r$/, "", line)
    if (line ~ /^[ \t]*(#|$)/) next
    equals = index(line, "=")
    if (!equals) settings_fail("E_SETTINGS_ASSIGNMENT")
    key = strip(substr(line, 1, equals - 1))
    if (key ~ /(^|_)(TOKEN|KEY|SECRET|PASSWORD)$/) settings_fail("E_SETTINGS_SECRET_NOT_ALLOWED")
    if (!(key in allowed)) settings_fail("E_SETTINGS_UNKNOWN_KEY")
    if (seen[key]++) settings_fail("E_SETTINGS_DUPLICATE_KEY")
    value = setting_value(substr(line, equals + 1))
    if (value ~ /[[:cntrl:]]/) settings_fail("E_SETTINGS_CONTROL")
    values[key] = value
}
END {
    if (bad) exit 2
    # Emit every allowed key, including absent keys, to prevent ambient shell
    # settings from hiding an incomplete project configuration.
    for (i = 1; i <= count; i++) print names[i] "=" values[names[i]]
}
