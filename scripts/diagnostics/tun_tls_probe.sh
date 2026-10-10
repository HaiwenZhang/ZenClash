#!/usr/bin/env bash
# Probe a fresh TLS connection without changing the active TUN or system proxy.
# Usage: bash scripts/diagnostics/tun_tls_probe.sh LABEL [--proxy-port PORT] [--count N] [--domain DOMAIN]

set -euo pipefail
umask 077

usage() {
  printf 'Usage: %s LABEL [--proxy-port PORT] [--count N] [--domain chatgpt.com|www.google.com]\n' "$0" >&2
  exit 2
}

[[ $# -ge 1 ]] || usage
label=$1
shift
[[ $label =~ ^[a-zA-Z0-9_-]+$ ]] || usage

proxy_port=''
attempts=3
domain=chatgpt.com
while [[ $# -gt 0 ]]; do
  [[ $# -ge 2 ]] || usage
  case $1 in
    --proxy-port)
      [[ -z $proxy_port && $2 =~ ^[0-9]+$ ]] || usage
      proxy_port=$2
      (( proxy_port >= 1 && proxy_port <= 65535 )) || usage
      ;;
    --count)
      [[ $2 =~ ^[0-9]+$ ]] || usage
      attempts=$2
      (( attempts >= 1 && attempts <= 20 )) || usage
      ;;
    --domain)
      [[ $2 == chatgpt.com || $2 == www.google.com ]] || usage
      domain=$2
      ;;
    *) usage ;;
  esac
  shift 2
done

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
capture_dir="$repo_root/target/diagnostics/tun-tls/$(date '+%Y%m%d-%H%M%S')-$label"
mkdir -p "$capture_dir"

log_file="$HOME/Library/Application Support/ZenClash/logs/mihomo.log"
log_offset=0
if [[ -f $log_file ]]; then
  log_offset=$(stat -f '%z' "$log_file")
fi

printf 'label=%s\n' "$label" > "$capture_dir/context.txt"
printf 'domain=%s\n' "$domain" >> "$capture_dir/context.txt"
if [[ -n $proxy_port ]]; then
  printf 'route=local-proxy:%s\n' "$proxy_port" >> "$capture_dir/context.txt"
else
  printf 'route=system-routing\n' >> "$capture_dir/context.txt"
fi
printf 'started_at=%s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" >> "$capture_dir/context.txt"
effective_config="$HOME/Library/Application Support/ZenClash/controlled-config/effective.yaml"
if [[ -f $effective_config ]]; then
  printf 'zenclash_config_sha256=%s\n' "$(shasum -a 256 "$effective_config" | cut -d ' ' -f 1)" >> "$capture_dir/context.txt"
  awk '/^log-level:/ {print "zenclash_log_level=" $2; exit}' "$effective_config" >> "$capture_dir/context.txt"
fi

if [[ -n $proxy_port ]]; then
  curl_route=(--noproxy '' --proxy "http://127.0.0.1:$proxy_port")
else
  curl_route=(--noproxy '*' --proxy '')
fi

successes=0
for ((attempt = 1; attempt <= attempts; attempt++)); do
  metrics="$capture_dir/attempt-$attempt.metrics"
  error_file="$capture_dir/attempt-$attempt.error"
  if curl "${curl_route[@]}" \
      --max-time 25 --connect-timeout 15 \
      --silent --show-error --output /dev/null \
      --write-out 'http=%{http_code}\nremote_ip=%{remote_ip}\nname_lookup_s=%{time_namelookup}\ntcp_connect_s=%{time_connect}\ntls_complete_s=%{time_appconnect}\ntotal_s=%{time_total}\n' \
      "https://$domain/robots.txt" > "$metrics" 2> "$error_file"; then
    curl_exit=0
  else
    curl_exit=$?
  fi
  printf 'curl_exit=%s\n' "$curl_exit" >> "$metrics"
  http_code=$(awk -F= '$1 == "http" {print $2}' "$metrics")
  if [[ $curl_exit -eq 0 && $http_code != 000 && -n $http_code ]]; then
    successes=$((successes + 1))
  fi
done

python3 - "$log_file" "$log_offset" "$capture_dir" "$domain" <<'PY'
from collections import Counter
from hashlib import sha256
from pathlib import Path
import re
import sys

log_file, offset, capture_dir = Path(sys.argv[1]), int(sys.argv[2]), Path(sys.argv[3])
domain = sys.argv[4]
routes = Counter()
warnings = 0
if log_file.is_file():
    with log_file.open('rb') as source:
        if source.seek(0, 2) >= offset:
            source.seek(offset)
        else:
            source.seek(0)  # The bounded log was compacted during the probe.
        for raw in source:
            line = raw.decode('utf-8', errors='replace').strip()
            if domain not in line.lower():
                continue
            match = re.search(r'\bmatch\s+(.+?)\s+using\s+(.+)$', line, re.I)
            if match:
                rule, outbound = match.groups()
                # Keep only the public test domain and a stable pseudonym for the
                # chosen outbound. Node names can contain private provider details.
                rule = re.sub(re.escape(domain), domain, rule, flags=re.I)
                if re.fullmatch(r'[A-Za-z()\s.-]*' + re.escape(domain) + r'[A-Za-z()\s.-]*', rule):
                    routes[(rule, sha256(outbound.encode()).hexdigest()[:12])] += 1
            elif re.search(r'\b(?:warn(?:ing)?|error)\b', line, re.I):
                warnings += 1

with (capture_dir / 'matched-routes.tsv').open('w') as output:
    output.write('rule\toutbound_id\tcount\n')
    for (rule, outbound_id), count in sorted(routes.items()):
        output.write(f'{rule}\t{outbound_id}\t{count}\n')
with (capture_dir / 'context.txt').open('a') as output:
    output.write(f'matched_route_events={sum(routes.values())}\n')
    output.write(f'target_warning_or_error_events={warnings}\n')

for error_file in sorted(capture_dir.glob('attempt-*.error')):
    message = error_file.read_text(errors='replace').strip()
    # The command uses a fixed public URL and an unauthenticated loopback proxy.
    # Bound output anyway in case curl adds unexpected transport details.
    message = re.sub(r'(?i)https?://[^\s@]+@', 'https://<REDACTED>@', message)
    error_file.write_text(message[:500] + ('\n' if message else ''))
PY

printf 'result=%s/%s TLS connections completed\n' "$successes" "$attempts"
printf 'matched_route_events=%s\n' "$(awk -F= '$1 == "matched_route_events" {print $2}' "$capture_dir/context.txt")"
printf 'capture_dir=%s\n' "$capture_dir"
if [[ $successes -ne $attempts ]]; then
  exit 1
fi
