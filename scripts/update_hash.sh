#!/bin/bash
# scripts/update_hash.sh
# Update package hash and metadata in openwrt-fttr (targeting latest nightly / release)

set -euo pipefail

# Helper function to print logs
log() {
    printf '[INFO] %s\n' "$*" >&2
}

log_err() {
    printf '[ERROR] %s\n' "$*" >&2
}

# Helper to perform curl with retries
curl_retry() {
    curl --fail --silent --show-error --location --retry 3 --retry-delay 2 "$@"
}

# Determine the directory of the script and find repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Switch to repo root
cd "$REPO_ROOT"

TEMP_MAKEFILE=""
cleanup() {
    if [[ -n $TEMP_MAKEFILE ]]; then
        rm -f -- "$TEMP_MAKEFILE"
    fi
}
trap cleanup EXIT

# Set up Auth header if GITHUB_TOKEN is available
AUTH_HEADER=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
    AUTH_HEADER=(-H "Authorization: token $GITHUB_TOKEN")
fi

get_latest_repo_commit() {
    local repo="$1"
    local branch="${2:-main}"
    
    log "获取 $repo ($branch) 的最新提交信息..."
    local api_url="https://api.github.com/repos/$repo/commits/$branch"
    local commit_info
    if commit_info=$(curl_retry "${AUTH_HEADER[@]}" "$api_url" 2>/dev/null); then
        local commit_metadata
        if commit_metadata=$(jq -er '[.sha, .commit.committer.date] | @tsv' <<< "$commit_info" 2>/dev/null); then
            local commit_sha commit_timestamp commit_date package_version
            IFS=$'\t' read -r commit_sha commit_timestamp <<< "$commit_metadata"
            if [[ $commit_sha =~ ^[[:xdigit:]]{40}$ ]] && \
                commit_date=$(date -u -d "$commit_timestamp" +%Y-%m-%d 2>/dev/null) && \
                package_version=$(date -u -d "$commit_timestamp" +%Y.%m.%d.%H%M%S 2>/dev/null); then
                printf '%s|%s|%s\n' "$commit_sha" "$commit_date" "$package_version"
                return 0
            fi
        fi
    fi
    
    # Fallback to git ls-remote if GitHub API is rate-limited or fails
    local ls_remote
    if ls_remote=$(git ls-remote "https://github.com/$repo.git" "refs/heads/$branch" | awk 'NR == 1 { print $1 }') && \
        [[ $ls_remote =~ ^[[:xdigit:]]{40}$ ]]; then
        printf '%s|%s|%s\n' \
            "$ls_remote" \
            "$(date -u +%Y-%m-%d)" \
            "$(date -u +%Y.%m.%d.%H%M%S)"
        return 0
    fi
    
    log_err "无法获取 $repo 的最新提交"
    return 1
}

update_luci_nightly() {
    local mk_path="${1:-luci-app-fttr/Makefile}"
    local repo="LazuliKao/luci-app-fttr"
    
    if [ ! -f "$mk_path" ]; then
        log_err "Makefile 不存在: $mk_path"
        return 1
    fi
    
    log "正在检查 $repo 的最新状态并更新 $mk_path..."
    
    # 获取前端仓库最新 commit
    local commit_info commit_sha commit_date package_version
    commit_info=$(get_latest_repo_commit "$repo" "main") || return 1
    IFS='|' read -r commit_sha commit_date package_version <<< "$commit_info"
    log "最新提交: $commit_sha ($commit_date; package version $package_version)"
    
    # 下载 Nightly 发布的校验和文件或直接下载 tarball 计算哈希
    local nightly_url="https://github.com/$repo/releases/download/nightly/luci-app-fttr-nightly.tar.gz"
    local sha256_url="${nightly_url}.sha256"
    local pkg_hash=""
    
    log "正在获取 $nightly_url 的哈希..."
    # 优先尝试直接读取发布包伴随的 .sha256 文件
    local sha_content
    if sha_content=$(curl_retry "${AUTH_HEADER[@]}" "$sha256_url" 2>/dev/null); then
        pkg_hash=$(awk 'NR == 1 { print $1 }' <<< "$sha_content" | tr -d '\r\n')
    fi
    
    # 若 .sha256 文件未就绪，则下载 tarball 实时计算
    if [[ ! $pkg_hash =~ ^[[:xdigit:]]{64}$ ]]; then
        log "直接下载 tarball 计算 SHA256..."
        if ! pkg_hash=$(curl_retry "$nightly_url" | sha256sum | awk 'NR == 1 { print $1 }'); then
            log_err "下载 Nightly 归档包失败"
            return 1
        fi
    fi
    
    if [[ ! $pkg_hash =~ ^[[:xdigit:]]{64}$ ]]; then
        log_err "计算哈希失败，获取到的哈希格式异常: '$pkg_hash'"
        return 1
    fi
    pkg_hash=${pkg_hash,,}
    
    log "最新 Nightly 哈希: $pkg_hash"
    
    # Ensure every field being changed exists exactly once before writing anything.
    local variable count
    for variable in PKG_VERSION PKG_SOURCE_DATE PKG_SOURCE_VERSION PKG_HASH PKG_SOURCE_URL; do
        count=$(awk -F ':=' -v key="$variable" \
            '$1 ~ "^[[:space:]]*" key "[[:space:]]*$" { count++ } END { print count + 0 }' "$mk_path")
        if [[ $count -ne 1 ]]; then
            log_err "期望 $mk_path 中恰有一个 $variable，实际为 $count 个"
            return 1
        fi
    done

    TEMP_MAKEFILE=$(mktemp "${mk_path}.tmp.XXXXXX")
    sed -E \
        -e "s|^[[:space:]]*PKG_VERSION[[:space:]]*:=.*$|PKG_VERSION:=$package_version|" \
        -e "s|^[[:space:]]*PKG_SOURCE_DATE[[:space:]]*:=.*$|PKG_SOURCE_DATE:=$commit_date|" \
        -e "s|^[[:space:]]*PKG_SOURCE_VERSION[[:space:]]*:=.*$|PKG_SOURCE_VERSION:=$commit_sha|" \
        -e "s|^[[:space:]]*PKG_HASH[[:space:]]*:=.*$|PKG_HASH:=$pkg_hash|" \
        -e "s|^[[:space:]]*PKG_SOURCE_URL[[:space:]]*:=.*$|PKG_SOURCE_URL:=https://github.com/$repo/releases/download/nightly/|" \
        "$mk_path" > "$TEMP_MAKEFILE"

    if ! bash "$SCRIPT_DIR/check_luci_package_metadata.sh" "$TEMP_MAKEFILE"; then
        return 1
    fi
    chmod --reference="$mk_path" "$TEMP_MAKEFILE"
    mv -- "$TEMP_MAKEFILE" "$mk_path"
    TEMP_MAKEFILE=""
    
    log "更新 $mk_path 完成！"
}

# 运行更新
update_luci_nightly "luci-app-fttr/Makefile"

log "所有包哈希更新检查完成！"
