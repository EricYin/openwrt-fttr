#!/usr/bin/env bash
# Validate LuCI package metadata before a nightly update is committed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MAKEFILE="${1:-$REPO_ROOT/luci-app-fttr/Makefile}"

fail() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

read_var() {
    local name="$1"
    local value
    value=$(awk -F ':=' -v key="$name" '
        $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
            count++
            value = $2
        }
        END {
            if (count != 1) exit 1
            sub(/^[[:space:]]*/, "", value)
            sub(/[[:space:]]*$/, "", value)
            print value
        }
    ' "$MAKEFILE") || fail "期望 $MAKEFILE 中恰有一个 $name"
    printf '%s' "$value"
}

[[ -f $MAKEFILE ]] || fail "Makefile 不存在: $MAKEFILE"

pkg_version=$(read_var PKG_VERSION)
pkg_release=$(read_var PKG_RELEASE)
pkg_source_date=$(read_var PKG_SOURCE_DATE)
pkg_source_version=$(read_var PKG_SOURCE_VERSION)
pkg_source=$(read_var PKG_SOURCE)
pkg_source_url=$(read_var PKG_SOURCE_URL)
pkg_hash=$(read_var PKG_HASH)

[[ $pkg_version =~ ^[0-9]+(\.[0-9]+)*$ ]] || \
    fail "PKG_VERSION 必须为 APK 兼容的纯数字版本，当前为 '$pkg_version'"
[[ $pkg_release =~ ^[0-9]+$ ]] || fail "PKG_RELEASE 必须为数字，当前为 '$pkg_release'"
[[ $pkg_source_date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || \
    fail "PKG_SOURCE_DATE 格式无效: '$pkg_source_date'"
normalized_source_date=$(date -u -d "$pkg_source_date" +%Y-%m-%d 2>/dev/null) || \
    fail "PKG_SOURCE_DATE 不是有效日期: '$pkg_source_date'"
[[ $normalized_source_date == "$pkg_source_date" ]] || \
    fail "PKG_SOURCE_DATE 不是有效日期: '$pkg_source_date'"
[[ $pkg_source_version =~ ^[[:xdigit:]]{40}$ ]] || \
    fail "PKG_SOURCE_VERSION 必须为 40 位 Git commit SHA，当前为 '$pkg_source_version'"
[[ $pkg_hash =~ ^[[:xdigit:]]{64}$ ]] || \
    fail "PKG_HASH 必须为 64 位 SHA-256，当前为 '$pkg_hash'"
[[ $pkg_source == '$(PKG_NAME)-nightly.tar.gz' ]] || \
    fail "PKG_SOURCE 必须指向 nightly 归档文件，当前为 '$pkg_source'"
[[ $pkg_source_url == 'https://github.com/LazuliKao/luci-app-fttr/releases/download/nightly/' ]] || \
    fail "PKG_SOURCE_URL 未指向 LuCI nightly 发布目录: '$pkg_source_url'"

printf '[INFO] LuCI package metadata is valid (version %s, source %s).\n' \
    "$pkg_version" "$pkg_source_version"
