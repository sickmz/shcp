#!/usr/bin/env bash
# Fake session for the README screenshot. Run it in a shcp terminal on a server you can use sudo on.
#   bash demo-server.sh          the folder, then a shell with a fake prompt
#   bash demo-server.sh logs     log lines that keep coming, for a second terminal
#   bash demo-server.sh clean    removes what it made
set -eu

ROOT=/srv/shop
RC=/tmp/shcp-demo-rc

green=$'\e[1;32m' blue=$'\e[1;34m' yellow=$'\e[33m' cyan=$'\e[36m' reset=$'\e[0m'
# The user the app shows in the card, so the prompt says the same.
who=$(id -un)
prompt="${green}${who}@shop-web-1${reset}:${blue}${ROOT}${reset}\$ "

if [ "${1:-}" = clean ]; then
    [ -e "$ROOT/.shcp-demo" ] && sudo rm -rf "$ROOT"
    rm -f "$RC"
    exit 0
fi

if [ "${1:-}" = logs ]; then
    printf '%s%s\n' "$prompt" 'tail -f logs/worker.log'
    jobs=(send_receipt sync_stock resize_image refresh_prices)
    while :; do
        printf '%s %sINFO%s  job %-15s done in %3dms\n' \
            "$(date +%H:%M:%S)" "$cyan" "$reset" "${jobs[RANDOM % 4]}" "$((RANDOM % 400 + 20))"
        sleep "$((RANDOM % 3 + 1))"
    done
fi

if [ ! -e "$ROOT" ]; then
    sudo mkdir -p "$ROOT"
    sudo chown "$(id -un)" "$ROOT"
    touch "$ROOT/.shcp-demo"
fi
cd "$ROOT"
mkdir -p current logs releases shared
for file in deploy.sh:2140 compose.yaml:1874 Caddyfile:612 README.md:4096 .env.example:380; do
    head -c "${file#*:}" /dev/zero | tr '\0' '#' > "${file%:*}"
done
touch -d yesterday current logs releases shared deploy.sh compose.yaml Caddyfile README.md

cat > "$RC" <<EOF
PS1='\[${green}\]${who}@shop-web-1\[${reset}\]:\[${blue}\]\w\[${reset}\]\\$ '
PROMPT_COMMAND='printf "\033]7;file://shop-web-1%s\033\\\\" "\$PWD"'
EOF

clear
printf '%s%s\n' "$prompt" 'systemctl status shop-api'
printf '%s●%s shop-api.service - Shop API\n' "$green" "$reset"
printf '     Loaded: loaded; enabled\n'
printf '     Active: %sactive (running)%s since Tue 08:12:40; 2 days ago\n' "$green" "$reset"
printf '   Main PID: 2317 (shop-api)\n'
printf '      Tasks: 14 (limit: 4557)\n'
printf '     Memory: 86.3M\n\n'
printf '%s%s\n' "$prompt" 'du -sh releases shared'
printf '1.2G\treleases\n340M\tshared\n\n'
printf '%s%s\n' "$prompt" 'tail -5 logs/app.log'
printf '14:02:11 %sINFO%s  POST /checkout       201  64ms\n' "$cyan" "$reset"
printf '14:02:13 %sINFO%s  GET  /products/118   200   7ms\n' "$cyan" "$reset"
printf '14:02:18 %sWARN%s  Slow query: 640ms (stock_by_store)\n' "$yellow" "$reset"
printf '14:02:20 %sINFO%s  GET  /health         200   1ms\n' "$cyan" "$reset"
printf '14:02:24 %sINFO%s  GET  /cart/42        200  11ms\n\n' "$cyan" "$reset"
exec bash --rcfile "$RC" -i
