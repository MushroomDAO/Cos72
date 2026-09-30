#!/usr/bin/env bash
# Cos72 发布打包脚本（T2.1.1 / Agent24 ME4-6.0.2 的 Cos72 部分，docs/agent/tasks.md）。
#
# 产出：
#   dist/cos72-<ver>-macos-arm64.tar.gz  —— 解包后顶层目录里恰好只有
#     domain-os.yml（仓库根目录那份）和 bin/cos72（release 二进制，可执行）。
#   dist/SHA256SUMS                      —— 在 dist/ 内对 tarball 跑
#     `shasum -a 256`，文件名写相对路径。
#
# 只在 Darwin arm64 上跑（目前只签发这一个平台的发布物）。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

uname_sm="$(uname -sm)"
if [[ "$uname_sm" != "Darwin arm64" ]]; then
  echo "error: scripts/package.sh only runs on Darwin arm64 (got: $uname_sm)" >&2
  exit 1
fi

# --- migrations 是否编译进了二进制？ ---
# src/store/mod.rs 用的是 `sqlx::migrate!("./migrations")` —— sqlx 的编译期宏，会把
# migrations/ 下每个 .sql 文件的内容在编译期读入并嵌进二进制（等价 include_str! 展开成
# 静态数组），运行时不再碰磁盘上的 migrations/ 目录。这与运行时形式
# `sqlx::migrate::Migrator::new(path).await` 不同 —— 后者才是运行时从磁盘读。
# 如果代码库改成了运行时形式，"包里只有两个文件" 这条约定就不成立了，必须停下来重新
# 设计打包内容（很可能要把 migrations/ 也塞进包里），而不是悄悄打出一个跑不起来的包。
if ! grep -rq 'sqlx::migrate!(' src/; then
  echo "error: 在 src/ 下没找到 sqlx::migrate!(...) 宏调用，无法确认 migrations 是否编译进了二进制。" >&2
  echo "       停止打包 —— 请先确认 migrations 的加载方式，再决定包内容是否只需要 domain-os.yml + bin/cos72。" >&2
  exit 1
fi
if grep -rq 'migrate::Migrator::new' src/; then
  echo "error: 在 src/ 下发现了 sqlx::migrate::Migrator::new(...)（运行时从磁盘加载 migrations），" >&2
  echo "       而不是编译期宏 sqlx::migrate!(...)。这意味着 migrations/ 目录可能不是编译进二进制的，" >&2
  echo "       \"包里只有两个文件\" 的约定不成立。停止打包，需要人工确认后再调整脚本。" >&2
  exit 1
fi

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')"
if [[ -z "${VERSION:-}" ]]; then
  echo "error: 无法从 Cargo.toml 读到 package version" >&2
  exit 1
fi

echo "==> packaging cos72 v${VERSION} for macos-arm64"

echo "==> cargo build --release"
cargo build --release

BIN="target/release/cos72"
if [[ ! -x "$BIN" ]]; then
  echo "error: 期望的 release 二进制 $BIN 不存在或不可执行" >&2
  exit 1
fi

if [[ ! -f "domain-os.yml" ]]; then
  echo "error: 仓库根目录下没有 domain-os.yml" >&2
  exit 1
fi

PKG_NAME="cos72-${VERSION}-macos-arm64"
DIST_DIR="dist"
STAGE_DIR="${DIST_DIR}/${PKG_NAME}"
TARBALL="${PKG_NAME}.tar.gz"

rm -rf "$STAGE_DIR" "${DIST_DIR:?}/${TARBALL}" "${DIST_DIR:?}/SHA256SUMS"
mkdir -p "${STAGE_DIR}/bin"

cp "$BIN" "${STAGE_DIR}/bin/cos72"
chmod +x "${STAGE_DIR}/bin/cos72"
cp domain-os.yml "${STAGE_DIR}/domain-os.yml"

echo "==> tar -czf ${DIST_DIR}/${TARBALL}"
( cd "$DIST_DIR" && tar -czf "$TARBALL" "$PKG_NAME" )

# 打包完就地清理暂存目录，dist/ 里只留 tarball 和 SHA256SUMS。
rm -rf "$STAGE_DIR"

echo "==> shasum -a 256 ${TARBALL} > SHA256SUMS"
( cd "$DIST_DIR" && shasum -a 256 "$TARBALL" > SHA256SUMS )

echo "==> self-check: shasum -a 256 -c SHA256SUMS"
( cd "$DIST_DIR" && shasum -a 256 -c SHA256SUMS )

echo "==> self-check: tar 内容恰好是 domain-os.yml 与 bin/cos72"
FILE_ENTRIES="$(cd "$DIST_DIR" && tar -tzf "$TARBALL" | grep -v '/$' | LC_ALL=C sort)"
EXPECTED_ENTRIES="$(printf '%s\n' "${PKG_NAME}/bin/cos72" "${PKG_NAME}/domain-os.yml" | LC_ALL=C sort)"

if [[ "$FILE_ENTRIES" != "$EXPECTED_ENTRIES" ]]; then
  echo "error: tar 内容与预期的两个文件不一致" >&2
  echo "--- 实际 ---" >&2
  echo "$FILE_ENTRIES" >&2
  echo "--- 预期 ---" >&2
  echo "$EXPECTED_ENTRIES" >&2
  exit 1
fi

echo "==> OK: ${DIST_DIR}/${TARBALL} + ${DIST_DIR}/SHA256SUMS"
