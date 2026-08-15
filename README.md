# stacksup

One command to run a Stacks stack — bitcoind, stacks-node, stacks-signer,
the Stacks Blockchain API, the Stacks Mesh API, and Postgres — from a single
config file.

```bash
stacksup config init      # write a stacks.toml
stacksup start [service]  # validate, render, start enabled services (--no-render to skip render)
stacksup status           # state of every service (managed and external)
stacksup config check     # config coherence + connectivity checks
stacksup logs [service]   # follow service logs
stacksup logs export      # shareable, redacted support bundle (logs + diagnostics)
stacksup stop [service]   # stop enabled services, keep containers+logs (--destroy to remove)
stacksup restart [service] # stop + start so config/image changes take effect
stacksup pull             # pull the latest images for every enabled service
stacksup upgrade          # check registries for newer image versions (suggestions only)
stacksup config render    # regenerate rendered/ without starting anything
stacksup chainstate wipe [service] # delete on-disk state, all or one service (asks first)
stacksup chainstate status # compare every service's chain tip (stacks + bitcoin heights)
stacksup chainstate download # seed chainstate from the Hiro Archive (resumable, verified)
```

## Networks

Standard network definitions (burnchain endpoint, chain id, epochs, seeded
balances, bootstrap peers) live in [`networks/`](networks/) and ship embedded
in the binary — `network = "testnet"` in stacks.toml references them by name.
A new testnet (new chain id, new epochs) is a new file there, not a config
migration. Unknown names resolve as custom definition files next to your
stacks.toml (`networks/<name>.toml`), so you can define private networks
without a tool release.

## The model

`stacks.toml` is the single source of truth. Every service has a `mode`:

| mode | meaning |
|---|---|
| `enabled` | run by this tool via docker compose |
| `external` | you run it elsewhere; we wire configs to it, health-check it, never touch it |
| `disabled` | not part of this stack (dependents fail validation) |

Everything lives under `--data-dir` (default: the current directory):
`rendered/` holds the compose file and generated service configs, and `chainstate/`
holds service state (chainstate, Postgres, signer db) as bind mounts — so the
whole stack sits where you said, ready to back up or relocate as one unit.

Everything under `rendered/` — the compose file, node `Config.toml`, signer
config, API env — is generated from `stacks.toml`. Cross-service invariants
(node↔signer auth token, event-observer endpoints, chain IDs) are correct by
construction because they derive from one file. If you outgrow this tool,
take `rendered/` and leave: it's plain compose + config files.

When the node is `external` but the API or signer is `enabled`, the node must
be configured to *push* to them; `stacksup config render` emits
`rendered/apply-to-your-node.toml` with the exact blocks to add on your side,
and `stacksup config check` verifies the loop is closed.

Containers are segmented across three docker networks so a compromised
API-side container has no route to the signer or to bitcoind's RPC
interface: `bitcoin` (bitcoind + node), `core` (node + signer), and
`services` (API, mesh API, Postgres). The node joins each network only when
it's present — everything talks to the node. bitcoind's RPC port is
published loopback-only (unless the node is external and needs it
off-host), so containers can't sidestep the split via
`host.docker.internal`; the P2P port stays open on purpose — it exists to
accept peers from anywhere.

## Running multiple deployments

One machine can host several stacks side by side. Give each deployment its
own directory (config + `--data-dir`), a distinct `name`, and a
`port_offset`:

```toml
name = "testnet-b"   # compose project + container prefix (default: "stacks")
port_offset = 100    # shifts every published HOST port; container-internal
                     # ports and service wiring never change
```

With `port_offset = 100` the node RPC publishes on 20543, the API on 4099,
postgres on 5532, and so on. The rendered compose file embeds the project
name, so `stacksup` commands (and bare `docker compose -f` runs) are always
scoped to the deployment whose directory you're in — `stop`, `logs`, and
`chainstate wipe` can't touch a neighbour.

## Secrets

Credentials never live in `stacks.toml` — the tool rejects them there. They go
in a `secrets.toml` beside it (plain text, mode 0600, gitignored), which is
merged into the config at load time:

```toml
[postgres]
password = "..."

[bitcoind]
rpc_user = "..."
rpc_password = "..."

[stacks-node]
auth_token = "..."
```

`stacksup config init` generates one with random values when none exists;
an existing `secrets.toml` is yours and is **never modified or overwritten**
(not even by `init --force`) — if a required value is missing, the tool errors
out with a paste-ready snippet of exactly what to add. The file must be
owner-only (`chmod 600`), and values must be 8–128 characters of printable
ASCII without spaces, quotes, backslashes, `$`, or backticks (they are
interpolated into rendered TOML/env/compose files). At render time the
Postgres password is delivered as a compose secret file and the bitcoind
credentials become a derived `-rpcauth` hash, so no plain-text secret appears
in `docker inspect`.

## Development

```bash
cargo run -- config init
cargo run -- config render
cargo build --release   # binary at target/release/stacksup
```

## Seeding chainstate

`stacksup chainstate download` fetches the node chainstate tarball and/or the
API's Postgres dump from the [Hiro Archive](https://docs.hiro.so/en/resources/archive/download-guide),
verifies sha256, and restores them (untar for the node, `pg_restore` for the
API). Downloads are resumable — Ctrl-C and re-run any time. Useful flags:
`--service node|api|all`, `--archive <file|url|path>` to pin a specific
archive, `--check-only` for a dry-run plan, `--yes` for unattended runs
(`nohup stacksup chainstate download --yes &`), `--no-verify`,
`--skip-version-check`, `--keep-archives`, and `--start` to render and
start the deployment as soon as the restore finishes (seed + boot in one
command: `stacksup chainstate download --yes --start`). Always resolves versioned archives (never -latest pointers); the archive's version must be ≤
the service's configured `version` in stacks.toml.

## Roadmap

- [ ] `status --watch`: live sync progress (bitcoind headers, node tip vs peers via `/v3/health`, API ingest lag) via bollard
- [ ] Snapshot seeding on first `up`: Hiro archive chainstate + matching API pg_dump, resumable, checksummed
- [ ] `doctor`: chain-id cross-checks, event-stream-flowing check, node↔signer auth verification
- [x] Secrets: user-owned `secrets.toml` overlay beside stacks.toml — pg password via compose secret file, bitcoind via rpcauth hash, node/signer auth token
- [ ] `upgrade`: image update with pre-upgrade pg backup, ordered restart, post-check
- [ ] `snapshot`: stop-consistent chainstate + pg_dump pairs with version metadata
- [ ] Profiles: `exchange` (readonly API replicas, pruned mode), richer `signer` (monitor-signers wiring)
- [ ] Release: `dist init` for GitHub Releases + Homebrew tap (`brew install ...`)
- [ ] Pin real image tags (stacks-core, signer, API, mesh API)
