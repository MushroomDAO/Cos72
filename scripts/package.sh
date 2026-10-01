#!/usr/bin/env bash
# Cos72 发布打包脚本（T2.1.1 / Agent24 ME4-6.0.2 的 Cos72 部分，docs/agent/tasks.md；
# 多平台支持见 Agent24 docs/Deployment/TASKS.md DEP-A4）。
#
# 产出：
#   dist/cos72-<ver>-<os>-<arch>.tar.gz  —— 解包后顶层目录里恰好只有
#     domain-os.yml（仓库根目录那份）和 bin/cos72（release 二进制，可执行）。
#   dist/SHA256SUMS                      —— 在 dist/ 内对 tarball 跑
#     `shasum -a 256`，文件名写相对路径。
#
# 支持 --target <triple> 指定构建目标，取值限定在下面四个（与 Agent24
# release.yml / DEP-A4 验收一致）：
#   aarch64-apple-darwin        -> macos-arm64
#   x86_64-apple-darwin         -> macos-x64
#   x86_64-unknown-linux-gnu    -> linux-x64
#   aarch64-unknown-linux-gnu   -> linux-arm64
# 不传 --target 时，默认取本机 `rustc -vV` 报告的 host triple（必须也在上面
# 四个之内，否则报错退出）——这是本脚本设计上只在"原生 runner 构建原生目标"
# 场景下使用，不做交叉编译；跨平台构建请在对应 target 的原生 runner 上跑本脚本。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TARGET="${2:-}"
      shift 2
      ;;
    --target=*)
      TARGET="${1#--target=}"
      shift
      ;;
    *)
      echo "error: 未知参数：$1" >&2
      echo "usage: $0 [--target <triple>]" >&2
      exit 1
      ;;
  esac
done

if [[ -z "$TARGET" ]]; then
  TARGET="$(rustc -vV | sed -n 's/^host: //p')"
  if [[ -z "$TARGET" ]]; then
    echo "error: 未传 --target，且无法从 \`rustc -vV\` 探测到 host triple" >&2
    exit 1
  fi
  echo "==> 未传 --target，使用本机 host triple：$TARGET"
fi

case "$TARGET" in
  aarch64-apple-darwin)
    PKG_OS="macos"
    PKG_ARCH="arm64"
    ;;
  x86_64-apple-darwin)
    PKG_OS="macos"
    PKG_ARCH="x64"
    ;;
  x86_64-unknown-linux-gnu)
    PKG_OS="linux"
    PKG_ARCH="x64"
    ;;
  aarch64-unknown-linux-gnu)
    PKG_OS="linux"
    PKG_ARCH="arm64"
    ;;
  *)
    echo "error: 不支持的 --target：$TARGET" >&2
    echo "       仅支持：aarch64-apple-darwin / x86_64-apple-darwin / x86_64-unknown-linux-gnu / aarch64-unknown-linux-gnu" >&2
    exit 1
    ;;
esac

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

echo "==> packaging cos72 v${VERSION} for ${PKG_OS}-${PKG_ARCH} (target: ${TARGET})"

echo "==> cargo build --release --target ${TARGET}"
cargo build --release --target "$TARGET"

BIN="target/${TARGET}/release/cos72"
if [[ ! -x "$BIN" ]]; then
  echo "error: 期望的 release 二进制 $BIN 不存在或不可执行" >&2
  exit 1
fi

if [[ ! -f "domain-os.yml" ]]; then
  echo "error: 仓库根目录下没有 domain-os.yml" >&2
  exit 1
fi

echo "==> self-check: 二进制架构"
case "$PKG_OS" in
  macos)
    archs="$(lipo -archs "$BIN")"
    echo "    lipo -archs: $archs"
    case "$PKG_ARCH" in
      arm64) echo "$archs" | grep -qw arm64 || { echo "error: $BIN 不含 arm64 架构（lipo -archs: $archs）" >&2; exit 1; } ;;
      x64)   echo "$archs" | grep -qw x86_64 || { echo "error: $BIN 不含 x86_64 架构（lipo -archs: $archs）" >&2; exit 1; } ;;
    esac
    ;;
  linux)
    file_out="$(file "$BIN")"
    echo "    file: $file_out"
    case "$PKG_ARCH" in
      arm64) echo "$file_out" | grep -qi "aarch64" || { echo "error: $BIN 不是 aarch64 架构（file: $file_out）" >&2; exit 1; } ;;
      x64)   echo "$file_out" | grep -Eqi "x86-64|x86_64" || { echo "error: $BIN 不是 x86_64 架构（file: $file_out）" >&2; exit 1; } ;;
    esac
    ;;
esac

PKG_NAME="cos72-${VERSION}-${PKG_OS}-${PKG_ARCH}"
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
