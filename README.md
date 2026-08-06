# stacks

One command to run a Stacks stack — bitcoind, stacks-node, stacks-signer,
the Stacks Blockchain API, the Stacks Mesh API, and Postgres — from a single
config file.

```bash
stacks config init      # write a stacks.toml
stacks start            # validate, render configs, start enabled services
stacks status           # state of every service (managed and external)
stacks config check     # config coherence + connectivity checks
stacks logs             # follow service logs
stacks stop             # stop enabled services (never touches external ones)
stacks config render    # regenerate rendered/ without starting anything
stacks chainstate wipe  # delete all service data (asks for confirmation)
stacks chainstate status # compare every service's chain tip (stacks + bitcoin heights)
```

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
be configured to *push* to them; `stacks config render` emits
`rendered/apply-to-your-node.toml` with the exact blocks to add on your side,
and `stacks config check` verifies the loop is closed.

## Development

```bash
cargo run -- config init
cargo run -- config render
cargo build --release   # binary at target/release/stacks
```

## Roadmap

- [ ] `status --watch`: live sync progress (bitcoind headers, node tip vs peers via `/v3/health`, API ingest lag) via bollard
- [ ] Snapshot seeding on first `up`: Hiro archive chainstate + matching API pg_dump, resumable, checksummed
- [ ] `doctor`: chain-id cross-checks, event-stream-flowing check, node↔signer auth verification
- [ ] Secrets: generated per-stack tokens/passwords in a gitignored env file (currently dev defaults — do not use on mainnet)
- [ ] `upgrade`: image update with pre-upgrade pg backup, ordered restart, post-check
- [ ] `snapshot`: stop-consistent chainstate + pg_dump pairs with version metadata
- [ ] Profiles: `exchange` (readonly API replicas, pruned mode), richer `signer` (monitor-signers wiring)
- [ ] Release: `dist init` for GitHub Releases + Homebrew tap (`brew install ...`)
- [ ] Pin real image tags (stacks-core, signer, API, mesh API)
