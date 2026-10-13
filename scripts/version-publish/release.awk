# Protocol operations over json.awk. All input strings arrive through files/env,
# never through awk -v (which interprets backslash escapes).
function trim(s) {
    sub(/^( |\\u0009|\\u000a|\\u000b|\\u000c|\\u000d|\\u00a0|\\u1680|\\u200[0-9a]|\\u2028|\\u2029|\\u202f|\\u205f|\\u3000|\\ufeff)+/, "", s)
    sub(/( |\\u0009|\\u000a|\\u000b|\\u000c|\\u000d|\\u00a0|\\u1680|\\u200[0-9a]|\\u2028|\\u2029|\\u202f|\\u205f|\\u3000|\\ufeff)+$/, "", s)
    return s
}
function version(s,    p, i) {
    s = trim(s)
    if (s !~ /^[0-9]+\.[0-9]+\.[0-9]+$/) die("E_VERSION")
    split(s, p, ".")
    for (i = 1; i <= 3; i++) if (p[i] + 0 > 99) die("E_VERSION")
    return (p[1] + 0) "." (p[2] + 0) "." (p[3] + 0)
}
function encoded(s,    p) { split(s, p, "."); return 10000000 + p[1] * 131072 + p[2] * 1024 + p[3] * 8 + 1 }
function ids() {
    project = ENVIRON["RELEASE_PROJECT_CODE"]; platform = ENVIRON["RELEASE_PLATFORM_CODE"]
    if (project !~ /^[A-Za-z][A-Za-z0-9_-]*$/ || length(project) > 64) die("E_PROJECT")
    if (platform !~ /^[A-Za-z0-9._-]+$/ || length(platform) > 32 || platform == "." || platform == "..") die("E_PLATFORM")
}
function url(s,    authority, port) {
    if (s !~ /^https?:\/\/[^/?#]+/ || units(s) > 16000) die("E_DOWNLOAD_URL")
    if (s ~ /[ ]|\\u00[01][0-9a-f]|\\u00a0|\\u1680|\\u200[0-9a]|\\u202[89f]|\\u205f|\\u3000|\\ufeff|\\u005c/) die("E_DOWNLOAD_URL")
    authority = s; sub(/^https?:\/\//, "", authority); sub(/[/?#].*$/, "", authority)
    sub(/^.*@/, "", authority)
    if (authority == "") die("E_DOWNLOAD_URL")
    if (substr(authority, 1, 1) == "[") {
        if (authority !~ /^\[[0-9A-Fa-f:.]+\](:[0-9]+)?$/) die("E_DOWNLOAD_URL")
        sub(/^\[[^]]+\]/, "", authority)
    } else {
        if (authority !~ /^[^:@]+(:[0-9]+)?$/) die("E_DOWNLOAD_URL")
        sub(/^[^:]+/, "", authority)
    }
    if (authority != "") { port = substr(authority, 2); if (port + 0 > 65535) die("E_DOWNLOAD_URL") }
    return s
}
function validate(r, tag,    i, key, n, v) {
    if (T[r] != "object") die("E_INPUT")
    for (i = 1; i <= N[r]; i++) {
        key = K[r, i]
        if (key !~ /^(version|versionCode|changelog|downloadUrl|extra|forceUpdate|overwrite)$/) die("E_FIELDS")
    }
    n = member(r, "version"); if (T[n] != "string") die("E_VERSION")
    v = version(V[n]); put(r, "version", str(v))
    n = member(r, "versionCode")
    if (n && (T[n] != "number" || V[n] + 0 != encoded(v))) die("E_VERSION_CODE")
    if (tag != "") { sub(/^v/, "", tag); if (version(body(tag)) != v) die("E_TAG") }
    n = member(r, "forceUpdate"); if (n && T[n] != "boolean") die("E_FORCE_UPDATE")
    n = member(r, "overwrite"); if (n && T[n] != "boolean") die("E_OVERWRITE")
    n = member(r, "changelog")
    if (n && T[n] != "null") {
        if (T[n] != "string" || units(V[n]) > 16000) die("E_CHANGELOG")
        if (trim(V[n]) == "") put(r, "changelog", node("null", "null"))
    }
    n = member(r, "downloadUrl")
    if (n && T[n] != "null") {
        if (T[n] != "string") die("E_DOWNLOAD_URL")
        V[n] = trim(V[n])
        if (V[n] == "") put(r, "downloadUrl", node("null", "null"))
        else url(V[n])
    }
    n = member(r, "extra")
    # Strings use canonical Unicode escapes internally, while emit writes UTF-8.
    # Count decoded UTF-16 units, retaining required JSON escape overhead.
    if (n && json_units(n) > 16000) die("E_EXTRA")
    return r
}
function string_json_units(s,    i, cp, total) {
    total = 2
    for (i = 1; i <= length(s); i++) {
        if (substr(s, i, 1) != "\\") { total++; continue }
        cp = hex4(substr(s, i + 2, 4)); i += 5
        if (cp == 34 || cp == 92 || cp == 8 || cp == 9 || cp == 10 || cp == 12 || cp == 13) total += 2
        else if (cp < 32) total += 6
        else total++
    }
    return total
}
function json_units(n,    i, total, key) {
    if (T[n] == "string") return string_json_units(V[n])
    if (T[n] != "array" && T[n] != "object") return length(emit(n))
    total = 2
    for (i = 1; i <= N[n]; i++) {
        if (i > 1) total++
        if (T[n] == "array") total += json_units(C[n, i])
        else { key = K[n, i]; total += string_json_units(key) + 1 + json_units(member(n, key)) }
    }
    return total
}
function field(n, key, type,    c) {
    c = member(n, key)
    if (T[c] != type) die("E_RESPONSE_FIELD")
    return c
}
function safe_get(n, path,    p, count, i) {
    count = split(path, p, ".")
    for (i = 1; i <= count; i++) { n = member(n, p[i]); if (!n) die("E_MISSING_FIELD") }
    if (T[n] == "string") return text(n)
    if (T[n] == "number") {
        if (V[n] + 0 == int(V[n] + 0) && V[n] + 0 >= 0 && V[n] + 0 < 9007199254740992) return sprintf("%.0f", V[n] + 0)
        return V[n]
    }
    if (T[n] == "boolean" || T[n] == "null") return V[n]
    die("E_EXPECT_SCALAR")
}
function check_record(r, expected,    v, copy, i, key) {
    if (T[r] != "object" || text(field(r, "projectCode", "string")) != project || text(field(r, "platformCode", "string")) != platform) die("E_REGISTRY_IDENTITY")
    v = text(field(r, "version", "string"))
    if (version(body(v)) != v || (expected != "" && v != expected)) die("E_REGISTRY_VERSION")
    if (V[field(r, "versionCode", "number")] + 0 != encoded(v)) die("E_REGISTRY_CODE")
    copy = node("object", "")
    for (i = 1; i <= N[r]; i++) {
        key = K[r, i]
        if (key == "projectCode" || key == "platformCode") continue
        if (key == "overwrite") die("E_REGISTRY_FIELDS")
        put(copy, key, member(r, key))
    }
    validate(copy, "")
    if (N[r] != 8 || !member(r, "changelog") || !member(r, "extra") || !member(r, "downloadUrl") || !member(r, "forceUpdate")) die("E_REGISTRY_FIELDS")
    if (emit(copy) != record_fields(r)) die("E_REGISTRY_NORMALIZATION")
    return encoded(v)
}
function record_fields(r,    n, i, key) {
    n = node("object", "")
    for (i = 1; i <= N[r]; i++) { key = K[r, i]; if (key != "projectCode" && key != "platformCode") put(n, key, member(r, key)) }
    return emit(n)
}
function registry_plan(request, old, latest, indexnode, outdir,    v, high, oldexists, i, entry, ev, ec, previous, found, seen, result, names, key, val, list, insert, outcome) {
    ids(); validate(request, ""); v = text(member(request, "version"))
    oldexists = T[old] != "null"; high = 0
    if (T[latest] != "null") high = check_record(latest, "")
    if (oldexists) check_record(old, v)
    if (T[indexnode] != "array") die("E_REGISTRY_INDEX")
    previous = 1e20
    for (i = 1; i <= N[indexnode]; i++) {
        entry = C[indexnode, i]
        ev = text(field(entry, "version", "string")); ec = V[field(entry, "versionCode", "number")] + 0
        if (N[entry] != 2 || version(body(ev)) != ev || ec != encoded(ev) || ec >= previous || seen[ev]++) die("E_REGISTRY_INDEX")
        previous = ec
        if (i == 1 && ec != high) die("E_REGISTRY_INDEX")
        if (ev == v) found = 1
    }
    if ((high == 0 && N[indexnode] != 0) || (high != 0 && N[indexnode] == 0) || found != oldexists) die("E_REGISTRY_INDEX")
    if (oldexists && encoded(v) == high && emit(old) != emit(latest)) die("E_REGISTRY_LATEST")
    result = node("object", "")
    put(result, "projectCode", str(project)); put(result, "platformCode", str(platform))
    put(result, "version", str(v)); put(result, "versionCode", node("number", encoded(v)))
    split("changelog downloadUrl extra forceUpdate", names, " ")
    for (i = 1; i <= 4; i++) {
        key = names[i]; val = member(request, key)
        if (!val && oldexists) val = member(old, key)
        if (!val) val = node(key == "forceUpdate" ? "boolean" : "null", key == "forceUpdate" ? "false" : "null")
        put(result, key, val)
    }
    if (T[member(result, "downloadUrl")] != "string") die("E_DOWNLOAD_REQUIRED")
    if (oldexists && emit(result) == emit(old)) outcome = "unchanged"
    else if (encoded(v) < high || (!oldexists && encoded(v) <= high)) die("E_VERSION_CONFLICT")
    else if (oldexists && V[member(request, "overwrite")] != "true") die("E_OVERWRITE_REQUIRED")
    else outcome = oldexists ? "overwritten" : "created"
    if (!oldexists) {
        list = node("array", ""); entry = node("object", "")
        put(entry, "version", str(v)); put(entry, "versionCode", node("number", encoded(v)))
        append(list, entry)
        for (i = 1; i <= N[indexnode]; i++) append(list, C[indexnode, i])
        indexnode = list
    }
    savejson(result, outdir "/record.json")
    savejson(encoded(v) >= high ? result : latest, outdir "/latest.json")
    savejson(indexnode, outdir "/index.json")
    print outcome > (outdir "/outcome"); close(outdir "/outcome")
}
function uri(s,    i, b, out, ch) {
    for (i = 1; i <= length(s); i++) {
        ch = substr(s, i, 1)
        out = out (ch ~ /^[A-Za-z0-9._~-]$/ ? ch : sprintf("%%%02X", ORD[ch]))
    }
    return out
}
function curl_escape(s,    i, c, out) {
    if (s ~ /[\r\n]/) die("E_CREDENTIAL")
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\\" || c == "\"") out = out "\\"
        out = out c
    }
    return out
}
BEGIN {
    op = ENVIRON["VP_OP"]; file = ENVIRON["VP_FILE"]; file2 = ENVIRON["VP_FILE2"]
    if (op == "uri") { print uri(ENVIRON["VP_VALUE"]); exit }
    if (op == "curl-config") {
        secret = ENVIRON["VP_SECRET"]
        print "silent\nshow-error\ngloboff\nconnect-timeout = 10"
        print "header = \"X-GitHub-Api-Version: 2026-03-10\""
        if (secret != "") print "header = \"" curl_escape(ENVIRON["VP_HEADER"] ": " secret) "\""
        exit
    }
    if (op == "make") {
        r = node("object", ""); put(r, "version", str(ENVIRON["RELEASE_VERSION"]))
        if (ENVIRON["RELEASE_CHANGELOG_FILE"] != "") put(r, "changelog", str(readfile(ENVIRON["RELEASE_CHANGELOG_FILE"])))
        if (ENVIRON["RELEASE_EXTRA_FILE"] != "") put(r, "extra", loadjson(ENVIRON["RELEASE_EXTRA_FILE"]))
        if (ENVIRON["RELEASE_DOWNLOAD_URL"] != "") put(r, "downloadUrl", str(ENVIRON["RELEASE_DOWNLOAD_URL"]))
        if (ENVIRON["RELEASE_FORCE_UPDATE"] != "") put(r, "forceUpdate", parse(ENVIRON["RELEASE_FORCE_UPDATE"]))
        ids(); print emit(validate(r, "")); exit
    }
    if (op == "version") { print version(body(ENVIRON["VP_VALUE"])); exit }
    if (op == "ref") {
        ref = ENVIRON["VP_VALUE"]
        if (ref !~ /^Release-[0-9]+\.[0-9]+\.[0-9]+$/) die("E_RELEASE_BRANCH")
        ref_version = version(substr(ref, 9))
        if (ref != "Release-" ref_version) die("E_RELEASE_BRANCH")
        print ref_version; exit
    }
    if (op == "git-tag") {
        tag = ENVIRON["VP_VALUE"]
        if (tag == "" || tag ~ /[^A-Za-z0-9._\/-]/ || tag ~ /\.\.|\/\.|^\.|\.$|\.lock(\/|$)|\/\/|^\/|\/$/) die("E_GIT_TAG")
        print tag; exit
    }
    if (op == "repository") {
        repo = ENVIRON["VP_VALUE"]
        if (repo !~ /^[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/) die("E_REPOSITORY")
        print repo; exit
    }
    r = loadjson(file)
    if (op == "get") { print safe_get(r, ENVIRON["VP_FIELD"]); exit }
    if (op == "canonical") { print emit(r); exit }
    if (op == "validate") { ids(); print emit(validate(r, ENVIRON["VP_TAG"])); exit }
    if (op == "url") {
        put(r, "downloadUrl", str(ENVIRON["VP_VALUE"])); ids(); print emit(validate(r, "")); exit
    }
    if (op == "xil-response") {
        ids(); expected = loadjson(file2); v = text(member(expected, "version"))
        if (T[r] != "object" || V[field(r, "code", "number")] + 0 != 0) die("E_XIL_RESPONSE")
        d = field(r, "data", "object"); outcome = text(field(r, "remark", "string"))
        if (text(field(d, "projectCode", "string")) != project || text(field(d, "platformCode", "string")) != platform || text(field(d, "version", "string")) != v || V[field(d, "versionCode", "number")] + 0 != encoded(v)) die("E_XIL_IDENTITY")
        if ((ENVIRON["VP_HTTP"] == "201" && outcome != "created") || (ENVIRON["VP_HTTP"] == "200" && outcome != "unchanged" && outcome != "overwritten")) die("E_XIL_OUTCOME")
        print outcome; exit
    }
    if (op == "plan") {
        registry_plan(r, loadjson(file2), loadjson(ENVIRON["VP_LATEST"]), loadjson(ENVIRON["VP_INDEX"]), ENVIRON["VP_DIR"]); exit
    }
    if (op == "tree") {
        d = node("object", ""); put(d, "base_tree", str(ENVIRON["VP_BASE_TREE"]))
        list = node("array", ""); split("record latest index", names, " ")
        for (i = 1; i <= 3; i++) {
            entry = node("object", "")
            path = ENVIRON["VP_PREFIX"] "/" (i == 1 ? "versions/" ENVIRON["VP_VERSION"] : names[i]) ".json"
            put(entry, "path", str(path)); put(entry, "mode", str("100644")); put(entry, "type", str("blob"))
            put(entry, "content", str(readfile(ENVIRON["VP_DIR"] "/" names[i] ".json"))); append(list, entry)
        }
        put(d, "tree", list); print emit(d); exit
    }
    if (op == "commit") {
        d = node("object", ""); put(d, "tree", str(ENVIRON["VP_TREE"]))
        put(d, "message", str("Publish " ENVIRON["RELEASE_PROJECT_CODE"] " " ENVIRON["RELEASE_PLATFORM_CODE"] " " ENVIRON["VP_VERSION"]))
        list = node("array", ""); append(list, str(ENVIRON["VP_HEAD"])); put(d, "parents", list)
        print emit(d); exit
    }
    if (op == "update-ref") {
        d = node("object", ""); put(d, "sha", str(ENVIRON["VP_SHA"])); put(d, "force", node("boolean", "false")); print emit(d); exit
    }
    if (op == "release-create") {
        d = node("object", ""); put(d, "tag_name", str(ENVIRON["VP_RELEASE_TAG"]))
        put(d, "target_commitish", str(ENVIRON["VP_TARGET"]))
        put(d, "name", str(ENVIRON["VP_RELEASE_TAG"]))
        val = member(r, "changelog"); put(d, "body", T[val] == "string" ? val : str(""))
        put(d, "draft", node("boolean", "true")); put(d, "prerelease", node("boolean", "false"))
        print emit(d); exit
    }
    if (op == "pick-release" || op == "pick-asset") {
        if (T[r] != "array") die("E_RESPONSE_ARRAY")
        matchcount = 0; selected = 0
        for (i = 1; i <= N[r]; i++) {
            entry = C[r, i]
            if (op == "pick-release") yes = (text(field(entry, "tag_name", "string")) == ENVIRON["VP_RELEASE_TAG"])
            else if (ENVIRON["VP_ASSET_NAME"] != "") yes = (text(field(entry, "name", "string")) == ENVIRON["VP_ASSET_NAME"])
            else yes = (text(field(entry, "browser_download_url", "string")) == ENVIRON["VP_VALUE"])
            if (yes) { selected = entry; matchcount++ }
        }
        if (matchcount > 1) die("E_AMBIGUOUS_RESPONSE")
        print (selected ? emit(selected) : "null"); exit
    }
    if (op == "count") { if (T[r] != "array") die("E_RESPONSE_ARRAY"); print N[r] + 0; exit }
    if (op == "array-join") {
        d = loadjson(file2)
        if (T[r] != "array" || T[d] != "array") die("E_RESPONSE_ARRAY")
        for (i = 1; i <= N[d]; i++) append(r, C[d, i])
        print emit(r); exit
    }
    die("E_OPERATION")
}
