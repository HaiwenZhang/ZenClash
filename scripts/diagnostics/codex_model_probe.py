#!/usr/bin/env python3
"""Probe the existing Codex login through either a local proxy or system routing.

This does not change capture settings, read the workspace, or call agent tools.
Only a fixed public test prompt and a redacted timing summary are retained.
"""

import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
import tomllib


def redact(message):
    message = re.sub(r"(?i)(bearer\s+|sk-)[A-Za-z0-9_./+\-=]+", r"\1<REDACTED>", message)
    message = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f-]{23,}", "<REDACTED-ID>", message, flags=re.I)
    return message[:600]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--route", choices=["tun", "proxy"], required=True)
    parser.add_argument("--proxy-port", type=int, default=7890)
    parser.add_argument("--timeout", type=int, default=90)
    args = parser.parse_args()
    if not 1 <= args.proxy_port <= 65535 or not 15 <= args.timeout <= 180:
        parser.error("invalid port or timeout")
    os.umask(0o077)
    config = tomllib.loads((Path.home() / ".codex/config.toml").read_text())
    env = os.environ.copy()
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "NO_PROXY", "no_proxy"]:
        env.pop(name, None)
    if args.route == "proxy":
        proxy = f"http://127.0.0.1:{args.proxy_port}"
        env.update(HTTP_PROXY=proxy, HTTPS_PROXY=proxy, ALL_PROXY=proxy, NO_PROXY="127.0.0.1,localhost")
    else:
        env.update(NO_PROXY="*", no_proxy="*")
    with tempfile.TemporaryDirectory(prefix="zenclash-codex-probe-") as directory:
        command = ["codex", "exec", "--ephemeral", "--skip-git-repo-check", "--json", "-s", "read-only", "-C", directory]
        for feature in ["hooks", "plugins", "multi_agent", "memories"]:
            command.extend(["--disable", feature])
        for name in config.get("mcp_servers", {}):
            if not re.fullmatch(r"[A-Za-z0-9_-]+", name):
                parser.error("MCP identifier cannot be safely overridden for this probe")
            command.extend(["-c", f"mcp_servers.{name}.enabled=false"])
        command.append("Reply with exactly OK. Do not call tools or read files.")
        started = time.monotonic()
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, start_new_session=True)
        print(f"codex_probe_started; route={args.route}; model={config.get('model', 'default')}", flush=True)
        try:
            output, errors = process.communicate(timeout=args.timeout)
            timed_out = False
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            output, errors = process.communicate()
            timed_out = True
    events, answers, messages = [], [], []
    for line in output.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        kind = event.get("type")
        events.append(kind)
        item = event.get("item", {})
        if kind == "item.completed" and item.get("type") == "agent_message":
            answers.append(item.get("text", ""))
        if kind in ["error", "turn.failed"]:
            messages.append(str(event.get("message", event.get("error", ""))))
    messages.extend(line for line in errors.splitlines() if re.search(r"(?i)websocket|reconnect|error|falling back", line))
    result = {
        "route": "system-routing-tun" if args.route == "tun" else f"local-http-proxy:{args.proxy_port}",
        "explicit_proxy": args.route == "proxy",
        "model": config.get("model", "default"),
        "exit": process.returncode,
        "timed_out": timed_out,
        "elapsed_s": round(time.monotonic() - started, 3),
        "events": events,
        "answer_ok": any(answer.strip() == "OK" for answer in answers),
        "transport_messages": [redact(message) for message in messages[:8]],
    }
    folder = Path(__file__).resolve().parents[2] / "target/diagnostics/codex"
    folder.mkdir(parents=True, exist_ok=True)
    capture = folder / f"{time.strftime('%Y%m%d-%H%M%S')}-{args.route}.json"
    capture.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, ensure_ascii=False), flush=True)
    print(f"capture_path={capture}", flush=True)
    return 0 if result["answer_ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
