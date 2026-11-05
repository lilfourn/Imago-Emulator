#!/bin/sh
# Re-probes Guide's source map, crates/emu-guide/sources.toml, the way
# docs/research/game-chat.md §5.1 did. Run by hand, not in CI: it reaches
# about fifty sites, a second apart.
#
# For each site it prints its domain, the map's reach, and then:
#   page  - what its front page (or a game's start page) gave: "ok" (200),
#           "bot check" (a Cloudflare wall), "no answer", or the HTTP code;
#   api   - for MediaWiki sites, whether api.php answers a search as JSON;
#           for JSON APIs, whether the base URL answers.
# A line ending in "!" differs from the map: a "fetch" site that no longer
# opens, a "search" site that now does, or an API that stopped answering.
# The app never tries to get past a bot check, and neither does this.
#
# Usage: scripts/guide/check-sources.sh   (DELAY=seconds between requests)
set -eu

root="$(cd "$(dirname "$0")/../.." && pwd)"
map="$root/crates/emu-guide/sources.toml"
agent='DS-Emulator-Guide/0.1 (+https://github.com/lilfourn/ds-emulator)'
delay="${DELAY:-1}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# One line per site, "site DOMAIN REACH API", then one per game start,
# "start DOMAIN PATH", each once, in the map's order. The map keeps one key
# per line, which is all this reads.
awk '
    function value(line) {
        sub(/^[^=]*= *"/, "", line)
        sub(/".*$/, "", line)
        return line
    }
    function flush() {
        if (domain != "") print "site", domain, reach, (api == "" ? "-" : api)
        domain = ""; reach = ""; api = ""
    }
    /^\[/ { flush() }
    /^domain = / { domain = value($0) }
    /^reach = / { reach = value($0) }
    /^api = / { api = value($0) }
    /start = "/ {
        start = $0
        sub(/.*start = "/, "", start)
        sub(/".*$/, "", start)
        starts[++count] = "start " domain " " start
    }
    END {
        flush()
        for (i = 1; i <= count; i++) print starts[i]
    }
' "$map" | awk '!seen[$0]++' >"$tmp/list"

# get URL: fetches URL into $tmp/body and prints what came back.
get() {
    sleep "$delay"
    code="$(curl -sL -o "$tmp/body" -w '%{http_code}' -A "$agent" --max-time 20 "$1" </dev/null || true)"
    if grep -qiE 'just a moment|cf-chl|challenge-platform' "$tmp/body" 2>/dev/null; then
        echo "bot check"
    elif [ "$code" = 200 ]; then
        echo "ok"
    elif [ -z "$code" ] || [ "$code" = 000 ]; then
        echo "no answer"
    else
        echo "HTTP $code"
    fi
}

# host DOMAIN: a host to probe for DOMAIN, with a known wiki for wildcards.
host() {
    case "$1" in
    '*.fandom.com'*) echo "community.fandom.com" ;;
    '*.miraheze.org'*) echo "meta.miraheze.org" ;;
    '*.'*) echo "www.${1#\*.}" | cut -d/ -f1 ;;
    *) echo "$1" | cut -d/ -f1 ;;
    esac
}

# path DOMAIN: the path part of DOMAIN, such as "/wikis" for ign.com/wikis.
path() {
    case "$1" in
    */*) echo "/${1#*/}" ;;
    *) echo "" ;;
    esac
}

differ=0
printf '%-30s %-10s %-11s %s\n' DOMAIN REACH PAGE API
while read -r kind domain reach api <&3; do
    [ "$kind" = site ] || continue
    site="$(host "$domain")"
    page="$(get "https://$site$(path "$domain")/")"
    answer="-"
    case "$reach" in
    mediawiki)
        if [ "$(get "https://$site$api?action=query&list=search&srsearch=walkthrough&srlimit=1&format=json")" = ok ] &&
            grep -q '"search"' "$tmp/body"; then
            answer="answers"
        else
            answer="no search"
        fi
        ;;
    api)
        if [ "$(get "$api")" = ok ] && head -c 1 "$tmp/body" | grep -q '{'; then
            answer="answers"
        else
            answer="no JSON"
        fi
        ;;
    esac
    mark=""
    case "$reach:$page:$answer" in
    fetch:ok:* | search:bot*:* | mediawiki:*:answers | api:*:answers) ;;
    *) mark=" !" ;;
    esac
    [ -z "$mark" ] || differ=$((differ + 1))
    printf '%-30s %-10s %-11s %s%s\n' "$domain" "$reach" "$page" "$answer" "$mark"
done 3<"$tmp/list"

echo
echo "Game starts:"
while read -r kind domain start <&3; do
    [ "$kind" = start ] || continue
    page="$(get "https://$(host "$domain")$start")"
    mark=""
    [ "$page" = ok ] || {
        mark=" !"
        differ=$((differ + 1))
    }
    printf '  %-40s %s%s\n' "$domain$start" "$page" "$mark"
done 3<"$tmp/list"

echo
if [ "$differ" -eq 0 ]; then
    echo "Every site answered as the map says."
else
    echo "$differ differ from the map (marked !)."
    exit 1
fi
