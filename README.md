# ZenClash

ZenClash is a native Mihomo client implemented in Rust with GPUI and
gpui-component. The workspace currently contains:

- `zenclash-core`: typed Mihomo HTTP/WebSocket APIs, managed core process,
  macOS system proxy integration, traffic/log monitors, and real integration
  tests.
- `zenclash-ui`: GPUI native window, Clash Party-style card sidebar, proxy
  selection and latency tests, connections, rules, providers, runtime settings,
  TUN controls, logs, traffic charts, local profile switching, ordered YAML
  overrides, Sub-Store connectivity, and a native status-bar traffic icon.

## Run against the supplied real profile

Use a real Mihomo executable. ZenClash will start it with
`examples/19facdf022b.yaml` when its controller is not already available.

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo run -p zenclash-ui --bin zenclash
```

To connect to an already-running controller instead:

```sh
ZENCLASH_CONTROLLER=http://127.0.0.1:9090 \
  ZENCLASH_CONFIG="$PWD/examples/19facdf022b.yaml" \
  cargo run -p zenclash-ui --bin zenclash
```

Optional environment variables are `ZENCLASH_SECRET`, `ZENCLASH_CONFIG`,
`ZENCLASH_MIHOMO_HOME`, and `ZENCLASH_NETWORK_SERVICE`.

The profile page can switch to another local YAML through the native file
picker. The override page recursively merges one or more YAML files in their
selected order and sends the generated payload directly to Mihomo; source files
are never rewritten.

ZenClash connects to an existing Sub-Store backend/frontend at
`http://127.0.0.1:38324` and `http://127.0.0.1:14122` by default. Override these
with `ZENCLASH_SUBSTORE_URL` and `ZENCLASH_SUBSTORE_FRONTEND_URL`.

## Real Mihomo integration test

The integration test never starts a mock controller. It launches the supplied
Mihomo binary, overrides only the controller address to isolate the test, loads
the real profile, and verifies version, runtime config, proxies, group switching,
rules, providers, connections, and the traffic WebSocket.

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  cargo test -p zenclash-core --test real_mihomo -- --ignored --nocapture
```

If the profile needs GeoIP data, point `ZENCLASH_INTEGRATION_HOME` at a Mihomo
home directory that already contains the downloaded data or allow Mihomo to
download it during the test.

## Build the macOS app bundle

```sh
ZENCLASH_MIHOMO_BINARY=/absolute/path/to/mihomo \
  scripts/build_macos_app.sh
open target/ZenClash.app
```

The bundle contains the selected real Mihomo binary and profile under
`Contents/Resources`; runtime data is stored in
`~/Library/Application Support/ZenClash/mihomo`.
