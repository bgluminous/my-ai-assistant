# Portable JSON tree. Run with LC_ALL=C; no gawk extensions or external commands.
# Strings are stored internally as canonical escaped ASCII, never as executable
# shell text. JSON output restores readable UTF-8 while retaining required escapes.
BEGIN {
    for (ji = 1; ji < 256; ji++) ORD[sprintf("%c", ji)] = ji
    HEX = "0123456789abcdef"
}
function die(code) { print code > "/dev/stderr"; exit 2 }
function readfile(path,    line, result, status) {
    result = ""
    while ((status = (getline line < path)) > 0) result = result line "\n"
    close(path)
    if (status < 0) die("E_READ")
    return result
}
function node(type, value,    n) { n = ++nodes; T[n] = type; V[n] = value; return n }
function hex4(s,    i, v, d) {
    if (length(s) != 4) die("E_JSON_ESCAPE")
    v = 0
    for (i = 1; i <= 4; i++) {
        d = index(HEX, tolower(substr(s, i, 1))) - 1
        if (d < 0) die("E_JSON_ESCAPE")
        v = v * 16 + d
    }
    return v
}
function escape_cp(cp) {
    if (cp >= 32 && cp < 127 && cp != 34 && cp != 92) return sprintf("%c", cp)
    if (cp <= 65535) return sprintf("\\u%04x", cp)
    cp -= 65536
    return sprintf("\\u%04x\\u%04x", 55296 + int(cp / 1024), 56320 + cp % 1024)
}
function utf8(cp) {
    if (cp == 0) die("E_NUL_OUTPUT")
    if (cp < 128) return sprintf("%c", cp)
    if (cp < 2048) return sprintf("%c%c", 192 + int(cp / 64), 128 + cp % 64)
    if (cp < 65536) return sprintf("%c%c%c", 224 + int(cp / 4096), 128 + int(cp / 64) % 64, 128 + cp % 64)
    return sprintf("%c%c%c%c", 240 + int(cp / 262144), 128 + int(cp / 4096) % 64, 128 + int(cp / 64) % 64, 128 + cp % 64)
}
function next_cp(    b, count, min, cp, i, q) {
    b = ORD[substr(JS, JP++, 1)]
    if (b < 128) return b
    if (b >= 194 && b <= 223) { count = 1; min = 128; cp = b - 192 }
    else if (b >= 224 && b <= 239) { count = 2; min = 2048; cp = b - 224 }
    else if (b >= 240 && b <= 244) { count = 3; min = 65536; cp = b - 240 }
    else die("E_UTF8")
    for (i = 0; i < count; i++) {
        q = ORD[substr(JS, JP++, 1)]
        if (q < 128 || q > 191) die("E_UTF8")
        cp = cp * 64 + q - 128
    }
    if (cp < min || cp > 1114111 || (cp >= 55296 && cp <= 57343)) die("E_UTF8")
    return cp
}
function string_value(    out, ch, cp, low) {
    out = ""; JP++
    while (JP <= length(JS)) {
        ch = substr(JS, JP, 1)
        if (ch == "\"") { JP++; return out }
        if (ch == "\\") {
            JP++; ch = substr(JS, JP++, 1)
            if (ch == "u") {
                cp = hex4(substr(JS, JP, 4)); JP += 4
                if (cp >= 55296 && cp <= 56319) {
                    if (substr(JS, JP, 2) != "\\u") die("E_SURROGATE")
                    low = hex4(substr(JS, JP + 2, 4)); JP += 6
                    if (low < 56320 || low > 57343) die("E_SURROGATE")
                    cp = 65536 + (cp - 55296) * 1024 + low - 56320
                } else if (cp >= 56320 && cp <= 57343) die("E_SURROGATE")
            } else if (ch == "\"") cp = 34
            else if (ch == "\\") cp = 92
            else if (ch == "/") cp = 47
            else if (ch == "b") cp = 8
            else if (ch == "f") cp = 12
            else if (ch == "n") cp = 10
            else if (ch == "r") cp = 13
            else if (ch == "t") cp = 9
            else die("E_JSON_ESCAPE")
        } else {
            cp = next_cp()
            if (cp < 32) die("E_JSON_CONTROL")
        }
        out = out escape_cp(cp)
    }
    die("E_JSON_STRING")
}
function whitespace() { while (substr(JS, JP, 1) ~ /^[ \t\r\n]$/) JP++ }
function member(n, key) { return ((n SUBSEP key) in M) ? M[n, key] : 0 }
function put(n, key, value) {
    if (!((n SUBSEP key) in M)) { K[n, ++N[n]] = key }
    M[n, key] = value
}
function append(n, value) { C[n, ++N[n]] = value }
function parse_value(depth,    ch, n, key, val, token) {
    if (depth > 64) die("E_JSON_DEPTH")
    whitespace(); ch = substr(JS, JP, 1)
    if (ch == "\"") return node("string", string_value())
    if (ch == "{" || ch == "[") {
        n = node(ch == "{" ? "object" : "array", ""); JP++; whitespace()
        if (substr(JS, JP, 1) == (ch == "{" ? "}" : "]")) { JP++; return n }
        while (1) {
            if (ch == "{") {
                if (substr(JS, JP, 1) != "\"") die("E_JSON_KEY")
                key = string_value(); whitespace()
                if (substr(JS, JP++, 1) != ":") die("E_JSON_COLON")
                if ((n SUBSEP key) in M) die("E_JSON_DUPLICATE_KEY")
                put(n, key, parse_value(depth + 1))
            } else append(n, parse_value(depth + 1))
            whitespace(); val = substr(JS, JP++, 1)
            if (val == (ch == "{" ? "}" : "]")) return n
            if (val != ",") die("E_JSON_SEPARATOR")
            whitespace()
        }
    }
    if (match(substr(JS, JP), /^(true|false|null)/)) {
        token = substr(JS, JP, RLENGTH); JP += RLENGTH
        return node(token == "null" ? "null" : "boolean", token)
    }
    if (match(substr(JS, JP), /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/)) {
        token = substr(JS, JP, RLENGTH); JP += RLENGTH
        return node("number", token)
    }
    die("E_JSON_VALUE")
}
function parse(s,    result) {
    JS = s; JP = 1; result = parse_value(0); whitespace()
    if (JP <= length(JS)) die("E_JSON_TRAILING")
    return result
}
function loadjson(path) { return parse(readfile(path)) }
function body(s,    save, pos, result, cp) {
    save = JS; pos = JP; JS = s; JP = 1; result = ""
    while (JP <= length(JS)) { cp = next_cp(); result = result escape_cp(cp) }
    JS = save; JP = pos
    return result
}
function str(s) { return node("string", body(s)) }
function quote(s) { return "\"" body(s) "\"" }
function decoded(s,    i, out, cp, low) {
    out = ""
    for (i = 1; i <= length(s); i++) {
        if (substr(s, i, 1) != "\\") { out = out substr(s, i, 1); continue }
        cp = hex4(substr(s, i + 2, 4)); i += 5
        if (cp >= 55296 && cp <= 56319) {
            low = hex4(substr(s, i + 3, 4)); i += 6
            cp = 65536 + (cp - 55296) * 1024 + low - 56320
        }
        out = out utf8(cp)
    }
    return out
}
function json_string(s,    i, out, cp, low) {
    out = "\""
    for (i = 1; i <= length(s); i++) {
        if (substr(s, i, 1) != "\\") { out = out substr(s, i, 1); continue }
        cp = hex4(substr(s, i + 2, 4)); i += 5
        if (cp >= 55296 && cp <= 56319) {
            low = hex4(substr(s, i + 3, 4)); i += 6
            cp = 65536 + (cp - 55296) * 1024 + low - 56320
        }
        if (cp == 34) out = out "\\\""
        else if (cp == 92) out = out "\\\\"
        else if (cp == 8) out = out "\\b"
        else if (cp == 9) out = out "\\t"
        else if (cp == 10) out = out "\\n"
        else if (cp == 12) out = out "\\f"
        else if (cp == 13) out = out "\\r"
        else if (cp < 32) out = out sprintf("\\u%04x", cp)
        else out = out utf8(cp)
    }
    return out "\""
}
function text(n) {
    if (T[n] != "string") die("E_EXPECT_STRING")
    return decoded(V[n])
}
function units(s,    i, count) {
    for (i = 1; i <= length(s); i++) { count++; if (substr(s, i, 1) == "\\") i += 5 }
    return count + 0
}
function numeric(s,    sign, parts, count, exponent, dot, digits) {
    sign = ""; if (substr(s, 1, 1) == "-") { sign = "-"; s = substr(s, 2) }
    count = split(s, parts, /[eE]/); exponent = (count == 2 ? parts[2] + 0 : 0)
    # Bound arithmetic on exponents, without rounding significant digits.
    if (exponent > 100000 || exponent < -100000) die("E_NUMBER_EXPONENT")
    digits = parts[1]; dot = index(digits, ".")
    if (dot) { exponent -= length(digits) - dot; gsub(/\./, "", digits) }
    sub(/^0+/, "", digits)
    if (digits == "") return "0"
    while (substr(digits, length(digits), 1) == "0") { digits = substr(digits, 1, length(digits) - 1); exponent++ }
    if (exponent == 0) return sign digits
    return sign digits "e" sprintf("%.0f", exponent)
}
# Comparisons and API payloads stay compact; saved registry files use two spaces.
function emit(n, pretty, indent,    out, i, j, keys, tmp, key, line, tail) {
    if (T[n] == "string") return json_string(V[n])
    if (T[n] == "number") return numeric(V[n])
    if (T[n] == "boolean" || T[n] == "null") return V[n]
    line = pretty ? "\n" indent "  " : ""
    tail = pretty && N[n] ? "\n" indent : ""
    if (T[n] == "array") {
        out = "["
        for (i = 1; i <= N[n]; i++) out = out (i > 1 ? "," : "") line emit(C[n, i], pretty, indent "  ")
        return out tail "]"
    }
    if (T[n] != "object") die("E_JSON_NODE")
    # Locale is C and keys have canonical escaped spellings. Force string ordering.
    for (i = 1; i <= N[n]; i++) keys[i] = K[n, i]
    for (i = 2; i <= N[n]; i++) {
        tmp = keys[i]; j = i - 1
        while (j >= 1 && ("k" keys[j]) > ("k" tmp)) { keys[j + 1] = keys[j]; j-- }
        keys[j + 1] = tmp
    }
    out = "{"
    for (i = 1; i <= N[n]; i++) {
        key = keys[i]
        out = out (i > 1 ? "," : "") line json_string(key) (pretty ? ": " : ":") emit(M[n, key], pretty, indent "  ")
    }
    return out tail "}"
}
function savejson(n, path) { print emit(n, 1, "") > path; close(path) }
