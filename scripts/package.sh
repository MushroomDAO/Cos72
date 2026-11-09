#!/usr/bin/env bash
# Cos72 发布打包脚本（T2.1.1 / Agent24 ME4-6.0.2 的 Cos72 部分，docs/agent/tasks.md；
# 多平台支持见 Agent24 docs/Deployment/TASKS.md DEP-A4）。
# S1-9（OS 包完整性，对齐 Agent24 / Sin90）：stage 之后、tar 之前签 macOS 可执行文件、
# 公证，再用 `agent24 package-integrity generate` 写入 files.json，并给 domain-os.yml
# 补上 integrity 段。签名会改二进制字节，必须发生在生成清单之前，否则 sha256 对不上，
# 安装时会被拒绝。
#
# 产出：
#   dist/cos72-<ver>-<os>-<arch>.tar.gz  —— 解包后顶层目录里恰好只有
#     domain-os.yml（仓库根目录那份，被 generate 改写，带 integrity 段）、
#     files.json（它引用的逐文件 sha256 清单）和 bin/cos72（release 二进制；
#     macOS 上为 Developer ID 签名并公证过的原生程序）。
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
#
# 版本只读 Cargo.toml 的 [package] version。结束时自检：SHA256SUMS、tar 条目
# 恰好是 domain-os.yml / files.json / bin/cos72、以及二进制架构。
#
# macOS 签名身份：COS72_SIGN_IDENTITY（默认 Developer ID Application: Huifeng Jiao）。
# 公证 keychain profile：COS72_NOTARY_PROFILE（默认 apple-notary-HYAP444FN9）。
# SKIP_SIGN=1 跳过签名（同时不公证）；SKIP_NOTARIZE=1 只跳过公证。CI 没有证书，
# 两条都设为 1；正式签名包在本机构建。
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
    -h|--help)
      echo "usage: $0 [--target <triple>]" >&2
      echo "  aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu" >&2
      exit 0
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
# 如果代码库改成了运行时形式，"包里只有 domain-os.yml、files.json 和 bin/cos72"
# 这条约定就不成立了，必须停下来重新设计打包内容（很可能要把 migrations/ 也塞进包里），
# 而不是悄悄打出一个跑不起来的包。
if ! grep -rq 'sqlx::migrate!(' src/; then
  echo "error: 在 src/ 下没找到 sqlx::migrate!(...) 宏调用，无法确认 migrations 是否编译进了二进制。" >&2
  echo "       停止打包 —— 请先确认 migrations 的加载方式，再决定包内容是否只需要 domain-os.yml + files.json + bin/cos72。" >&2
  exit 1
fi
if grep -rq 'migrate::Migrator::new' src/; then
  echo "error: 在 src/ 下发现了 sqlx::migrate::Migrator::new(...)（运行时从磁盘加载 migrations），" >&2
  echo "       而不是编译期宏 sqlx::migrate!(...)。这意味着 migrations/ 目录可能不是编译进二进制的，" >&2
  echo "       \"包里只有 domain-os.yml、files.json 与 bin/cos72\" 的约定不成立。停止打包，需要人工确认后再调整脚本。" >&2
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

# 清掉上一次的产物，避免旧 tarball / SHA256SUMS 留在 dist/ 里。
rm -rf "$DIST_DIR"
mkdir -p "${STAGE_DIR}/bin"

# 失败时不留半成品：公证 zip、暂存目录（generate 之后属主只读）、半成品 tar 和
# SHA256SUMS。notarize-submission.json 留下以便排查。保留原退出码。
cleanup_on_failure() {
  local status=$?
  set +e
  if [[ "$status" -ne 0 ]]; then
    rm -f "${DIST_DIR}/.cos72-notarize.zip" "${DIST_DIR}/${TARBALL}" "${DIST_DIR}/SHA256SUMS" \
      || echo "warning: 无法删除 ${DIST_DIR} 里的半成品" >&2
    if [[ -d "$STAGE_DIR" ]]; then
      { chmod -R u+w "$STAGE_DIR" && rm -rf "$STAGE_DIR"; } \
        || echo "warning: 无法删除 ${STAGE_DIR}" >&2
    fi
  fi
  exit "$status"
}
trap cleanup_on_failure EXIT

cp "$BIN" "${STAGE_DIR}/bin/cos72"
chmod +x "${STAGE_DIR}/bin/cos72"
cp domain-os.yml "${STAGE_DIR}/domain-os.yml"

# S1-9：顺序固定。会改字节的步骤（签名、公证）在生成清单之前；清单在 tar 之前。
# 先 generate 再签名会改变 bin/cos72 的 sha256，扫描时包会被拒绝。
if [[ "$PKG_OS" == "macos" && "${SKIP_SIGN:-0}" != "1" ]]; then
  sign_identity="${COS72_SIGN_IDENTITY:-Developer ID Application: Huifeng Jiao (HYAP444FN9)}"
  notary_profile="${COS72_NOTARY_PROFILE:-apple-notary-HYAP444FN9}"
  echo "==> signing bin/cos72 (Developer ID, Hardened Runtime)"
  codesign --force --options runtime --timestamp --sign "$sign_identity" "${STAGE_DIR}/bin/cos72"
  codesign --verify --strict --verbose=1 "${STAGE_DIR}/bin/cos72"
  if [[ "${SKIP_NOTARIZE:-0}" != "1" ]]; then
    echo "==> notarizing bin/cos72"
    notary_zip="${DIST_DIR}/.cos72-notarize.zip"
    ditto -c -k --keepParent "${STAGE_DIR}/bin/cos72" "$notary_zip"
    # 裸 Mach-O 不能 staple；Gatekeeper 在线查询票据。
    xcrun notarytool submit "$notary_zip" --keychain-profile "$notary_profile" --wait \
      --output-format json > "${DIST_DIR}/notarize-submission.json"
    rm -f "$notary_zip"
    grep -q '"status" *: *"Accepted"' "${DIST_DIR}/notarize-submission.json" || {
      echo "error: 公证未被接受，见 ${DIST_DIR}/notarize-submission.json" >&2
      exit 1
    }
  fi
elif [[ "$PKG_OS" == "macos" ]]; then
  echo "warning: SKIP_SIGN=1 — 这个 macOS 包未签名；签名强制之后 Agent24 会拒绝它" >&2
fi

# 完整性清单由本包所对齐的那一版 Agent24 CLI 生成（AGENT24_CLI，默认 PATH 上的 agent24）。
# 它写出 files.json，改写 domain-os.yml 的 integrity 段，并设置 Agent24 检查的权限
# （目录 0700，bin/cos72 0500，其余 0400）。
agent24_cli="${AGENT24_CLI:-agent24}"
if [[ "$agent24_cli" != /* ]]; then
  command -v "$agent24_cli" >/dev/null 2>&1 || {
    echo "error: AGENT24_CLI '$agent24_cli' 找不到；它必须是带 package-integrity 的 Agent24 CLI" >&2
    exit 1
  }
elif [[ ! -x "$agent24_cli" ]]; then
  echo "error: AGENT24_CLI '$agent24_cli' 找不到；它必须是带 package-integrity 的 Agent24 CLI" >&2
  exit 1
fi
echo "==> generating the integrity manifest with $agent24_cli"
# 跨 CPU 打包（例如在 Apple 芯片 Mac 上打 x86_64-apple-darwin 包）时，告诉 CLI
# 可执行文件的目标架构。同架构打包不传参数，CI 里钉的旧版 Agent24 CLI（没有 --arch）照常可用。
target_cpu="${TARGET%%-*}"
case "$(uname -m)" in
  arm64|aarch64) host_cpu=aarch64 ;;
  x86_64|amd64)  host_cpu=x86_64 ;;
  *)             host_cpu="$(uname -m)" ;;
esac
arch_args=()
if [[ "$target_cpu" != "$host_cpu" ]]; then
  arch_args=(--arch "$target_cpu")
fi
"$agent24_cli" package-integrity generate --root "$STAGE_DIR" --executable bin/cos72 ${arch_args[@]+"${arch_args[@]}"}
"$agent24_cli" package-integrity verify --root "$STAGE_DIR" ${arch_args[@]+"${arch_args[@]}"}

echo "==> tar -czf ${DIST_DIR}/${TARBALL}"
# COPYFILE_DISABLE：macOS tar 否则会为扩展属性写入 AppleDouble `._*` 条目，
# 完整性检查会拒绝这些多出来的文件。
( cd "$DIST_DIR" && COPYFILE_DISABLE=1 tar -czf "$TARBALL" "$PKG_NAME" )

# 暂存目录只为喂给 tar；产物是 tarball。generate 把它改成属主只读（0400/0500/0700），
# 删除前先放开写权限。
chmod -R u+w "$STAGE_DIR"
rm -rf "$STAGE_DIR"

echo "==> shasum -a 256 ${TARBALL} > SHA256SUMS"
( cd "$DIST_DIR" && shasum -a 256 "$TARBALL" > SHA256SUMS )

echo "==> self-check: shasum -a 256 -c SHA256SUMS"
( cd "$DIST_DIR" && shasum -a 256 -c SHA256SUMS )

echo "==> self-check: tar 内容恰好是 domain-os.yml、files.json 与 bin/cos72"
FILE_ENTRIES="$(cd "$DIST_DIR" && tar -tzf "$TARBALL" | grep -v '/$' | LC_ALL=C sort)"
EXPECTED_ENTRIES="$(printf '%s\n' \
  "${PKG_NAME}/bin/cos72" \
  "${PKG_NAME}/domain-os.yml" \
  "${PKG_NAME}/files.json" | LC_ALL=C sort)"

if [[ "$FILE_ENTRIES" != "$EXPECTED_ENTRIES" ]]; then
  echo "error: tar 内容与预期的文件不一致" >&2
  echo "--- 实际 ---" >&2
  echo "$FILE_ENTRIES" >&2
  echo "--- 预期 ---" >&2
  echo "$EXPECTED_ENTRIES" >&2
  exit 1
fi

echo "==> OK: ${DIST_DIR}/${TARBALL} + ${DIST_DIR}/SHA256SUMS"
