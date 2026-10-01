# Cos72

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Inspired by AAStar Cos72, we create a new one for communities~!

## Development

`cargo test` runs the unit/integration suite (schema, manifest, kernel
wiring, structure boundaries). It never touches a real Agent24 daemon.

### Real-mount black box (`tests/agent24_mount_blackbox.rs`)

`#[ignore]`d so it never runs under plain `cargo test` / CI — it needs
sibling checkouts of Agent24 (and, from T1.4.1 onward, Sin90) with their own
binaries built from source. Run it explicitly:

```sh
# Cos72 alone (T1.1.1–T1.3.1 cases): needs a sibling Agent24 checkout.
AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24 \
  cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1

# Cos72 + Sin90 co-mounted on the SAME real agent24d (T1.4.1's own two
# cases, `cos72_full_flow_real_mount` / `cos72_and_sin90_coexist_isolated`):
# additionally needs a sibling Sin90 checkout already migrated to
# agent24-os-sdk (ME4-5.2.1). Missing SIN90_CHECKOUT is a hard panic, not a
# skip, for these two cases — there is no default guess.
AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24 \
  SIN90_CHECKOUT=$HOME/Dev/auraai/sin90-design \
  cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1
```

`--features test-hooks` is required for the whole invocation — several
cases (from T1.3.1b onward) poll Cos72's own `test-hooks`-only `POST
/debug/memory-recall` route, and the two T1.4.1 cases additionally drive
Sin90's own `test-hooks`-only `POST /debug/kernel-roundtrip` route (built
into a SEPARATE `sin90` binary under `$SIN90_CHECKOUT/target/test-hooks-debug`,
so it never collides with a plain `cargo build` of Sin90). `--test-threads=1`
because every case starts its own real `agent24d` subprocess bound to a
throwaway `$HOME` under `/tmp` — they do not share state, but running them
concurrently wastes CPU racing several real daemons for no benefit.

`cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --list`
lists every case without running any of them (useful to confirm the file
still compiles, or to check the count docs/agent/tasks.md's own acceptance
commands expect, without paying for a real mount).

## 安装发布包

发布包覆盖四个平台，按自己的系统 + 架构选择对应文件名（`<os>` 是
`macos`/`linux`，`<arch>` 是 `arm64`/`x64`）：

| 平台 | 包名 |
|---|---|
| macOS Apple Silicon | `cos72-<版本>-macos-arm64.tar.gz` |
| macOS Intel | `cos72-<版本>-macos-x64.tar.gz` |
| Linux x86_64 | `cos72-<版本>-linux-x64.tar.gz` |
| Linux arm64 | `cos72-<版本>-linux-arm64.tar.gz` |

从 Release 下载对应包和 `SHA256SUMS`，放在同一目录：

```sh
shasum -a 256 -c SHA256SUMS
tar -xzf cos72-<版本>-<os>-<arch>.tar.gz
agent24 os install cos72-<版本>-<os>-<arch>/
```

（发布包由 `scripts/package.sh` 产出：解包后的目录里只有 `domain-os.yml` 和
`bin/cos72` 两个文件 —— migrations 在编译期已经嵌进了二进制。本地打包时用
`--target <triple>` 指定目标三元组，支持 `aarch64-apple-darwin` /
`x86_64-apple-darwin` / `x86_64-unknown-linux-gnu` /
`aarch64-unknown-linux-gnu`；不传则默认本机平台。CI 中四个目标各自在原生
runner 上构建，见 `.github/workflows/release.yml`。）

## License

Licensed under the [Apache License, Version 2.0](https://opensource.org/licenses/Apache-2.0). See [LICENSE](./LICENSE) for details.
