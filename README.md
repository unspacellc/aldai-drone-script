# unspace

`unspace` is a Rust CLI foundation for the Unspace drone dock ingest agent. It is intended to run 24/7 on ARM64 Linux drone docks, manage a versioned config file, run as a hardened systemd service, and provide the command surface needed for video ingest work.

## Implemented foundation

- Clap command routing for `install`, `uninstall`, `watch`, `status`, `logs`, `update`, `healthcheck`, `config show`, and `config set`.
- Versioned JSON config at `/etc/unspace/config.json` by default.
- `UNSPACE_CONFIG_PATH` config path override.
- `UNSPACE_API_KEY` secret override.
- API key validation requiring a `ysk_` prefix.
- `config show` redacts the API key.
- `config set <key> <value>` validates and writes the config, then sends `SIGHUP` to `/var/run/unspace.pid` when the service is running.
- `watch` writes `/var/run/unspace.pid`, writes the local heartbeat file, catches `SIGHUP`, and reloads the currently used config without restarting.
- `install` writes the initial config from `--api-key` and `--dock-id`, creates Unspace working directories, writes a hardened systemd unit, and starts the service.
- `healthcheck` validates the watch directory, heartbeat freshness, and API `/health` reachability.
- Tag-push CI builds a stripped ARM64 Linux musl binary for the dock hardware.

The previous Python prototype is still present under `src/drone_dock_agent/` for reference while the Rust CLI is built out.

## Python prototype parity

No. The Rust CLI is a deployment and service-management scaffold, not a 1:1 port of the Python prototype yet. The Rust implementation currently covers CLI routing, config validation/redaction, systemd install scaffolding, PID/heartbeat handling, SIGHUP config reload, and health checks.

The Python prototype still contains ingest-agent behavior that has not been ported to Rust yet:

- filesystem event watching and startup backfill
- video extension filtering and file stability checks
- presigned upload URL requests and S3 POST uploads
- ingest-mission API calls
- KMZ resolution and filename metadata parsing
- WPML heading extraction from KMZ archives
- mission name, `track_group`, and `track_label` generation

Keep `src/drone_dock_agent/` as the functional reference implementation until those features are ported.

## Build and test

```bash
cargo test
cargo build --release
# Release CI builds the dock target: aarch64-unknown-linux-musl
```

## Configuration

Default path: `/etc/unspace/config.json`

Override path:

```bash
export UNSPACE_CONFIG_PATH=/tmp/unspace.config.json
```

Override API key without storing it in JSON:

```bash
export UNSPACE_API_KEY=ysk_KEY
```

Example config:

```json
{
  "config_version": 1,
  "api_base_url": "https://api.unspace.com",
  "api_key": "",
  "dock_id": "DOCK_42",
  "watch_dir": "/var/lib/unspace/uploads",
  "heartbeat_file": "/tmp/unspace.heartbeat",
  "heartbeat_interval_secs": 15,
  "heartbeat_max_age_secs": 120
}
```

## CLI examples

```bash
unspace --version
unspace install --api-key ysk_KEY --dock-id DOCK_42
unspace config show
unspace config set watch_dir /mnt/drone-footage
unspace healthcheck
unspace status
unspace logs
```

## Notes

The config intentionally contains only fields used by the current scaffold: API/dock identity, watch directory, and local heartbeat/healthcheck settings. Upload pipeline fields such as video extension filters, mission naming, model options, queue limits, and polling intervals will be added when those features are implemented. The `update` command is intentionally scaffolded and returns a clear not-implemented error rather than silently doing partial work.
