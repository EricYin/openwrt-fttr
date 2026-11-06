#!/bin/bash
# scripts/update_hash.sh
# Update package hash and metadata in openwrt-fttr (targeting latest nightly / release)

set -e

# Helper function to print logs
log() {
    echo -e "[INFO] $*" >&2
}

log_err() {
    echo -e "[ERROR] $*" >&2
}

# Helper to perform curl with retries
curl_retry() {
    curl -fsSL --retry 3 --retry-delay 2 "$@"
}

# Determine the directory of the script and find repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Switch to repo root
cd "$REPO_ROOT"

# Set up Auth header if GITHUB_TOKEN is available
AUTH_HEADER=()
if [ -n "$GITHUB_TOKEN" ]; then
    AUTH_HEADER=(-H "Authorization: token $GITHUB_TOKEN")
fi

get_latest_repo_commit() {
    local repo="$1"
    local branch="${2:-main}"
    
    log "获取 $repo ($branch) 的最新提交信息..."
    local commit_info
    if commit_info=$(curl_retry "${AUTH_HEADER[@]}" "https://api.github.com/repos/$repo/commits/$branch" 2>/dev/null); then
        local commit_sha
        commit_sha=$(echo "$commit_info" | jq -r '.sha' 2>/dev/null)
        local commit_date
        commit_date=$(echo "$commit_info" | jq -r '.commit.committer.date' 2>/dev/null | cut -c 1-10)
        
        if [ -n "$commit_sha" ] && [ "$commit_sha" != "null" ]; then
            echo "$commit_sha|${commit_date:-$(date +%Y-%m-%d)}"
            return 0
        fi
    fi
    
    # Fallback to git ls-remote if GitHub API is rate-limited or fails
    local ls_remote
    ls_remote=$(git ls-remote "https://github.com/$repo.git" "refs/heads/$branch" | awk '{print $1}')
    if [ -n "$ls_remote" ]; then
        echo "$ls_remote|$(date +%Y-%m-%d)"
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
    local commit_info
    local commit_sha=""
    local commit_date="$(date +%Y-%m-%d)"
    if commit_info=$(get_latest_repo_commit "$repo" "main"); then
        commit_sha=$(echo "$commit_info" | cut -d'|' -f1)
        commit_date=$(echo "$commit_info" | cut -d'|' -f2)
        log "最新提交: $commit_sha ($commit_date)"
    fi
    
    # 下载 Nightly 发布的校验和文件或直接下载 tarball 计算哈希
    local nightly_url="https://github.com/$repo/releases/download/nightly/luci-app-fttr-nightly.tar.gz"
    local sha256_url="${nightly_url}.sha256"
    local PKG_HASH=""
    
    log "正在获取 $nightly_url 的哈希..."
    # 优先尝试直接读取发布包伴随的 .sha256 文件
    local sha_content
    if sha_content=$(curl_retry "${AUTH_HEADER[@]}" "$sha256_url" 2>/dev/null); then
        PKG_HASH=$(echo "$sha_content" | awk '{print $1}' | tr -d '\r\n')
    fi
    
    # 若 .sha256 文件未就绪，则下载 tarball 实时计算
    if [ -z "$PKG_HASH" ] || [ ${#PKG_HASH} -ne 64 ]; then
        log "直接下载 tarball 计算 SHA256..."
        if ! PKG_HASH=$(curl_retry "$nightly_url" | sha256sum | awk '{print $1}'); then
            log_err "下载 Nightly 归档包失败"
            return 1
        fi
    fi
    
    if [ -z "$PKG_HASH" ] || [ ${#PKG_HASH} -ne 64 ]; then
        log_err "计算哈希失败，获取到的哈希格式异常: '$PKG_HASH'"
        return 1
    fi
    
    log "最新 Nightly 哈希: $PKG_HASH"
    
    # 更新 Makefile
    sed -i -E "s/^(PKG_VERSION\s*:=\s*).*/\1nightly/" "$mk_path"
    sed -i -E "s/^(PKG_SOURCE_DATE\s*:=\s*).*/\1$commit_date/" "$mk_path"
    if [ -n "$commit_sha" ]; then
        sed -i -E "s/^(PKG_SOURCE_VERSION\s*:=\s*).*/\1$commit_sha/" "$mk_path"
    fi
    sed -i -E "s/^(PKG_HASH\s*:=\s*).*/\1$PKG_HASH/" "$mk_path"
    sed -i -E "s|^(PKG_SOURCE_URL\s*:=\s*).*|\\1https://github.com/$repo/releases/download/nightly/|" "$mk_path"
    
    log "更新 $mk_path 完成！"
    echo ""
}

# 运行更新
update_luci_nightly "luci-app-fttr/Makefile"

log "所有包哈希更新检查完成！"
