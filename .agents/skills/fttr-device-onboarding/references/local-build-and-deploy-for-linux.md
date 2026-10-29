# 本地交叉构建与部署（只改 userspace）

本参考按 2026-10-28 的现场条件整理：主猫是 aarch64 OpenWrt，本轮只改 `h3c-fttr-tools`。
产物是**临时验证件**，不是交付固件；正式发布仍按 `README.md` 用 feed 构建。

## 何时使用

- 需要验证一个属性、一个指针或一个门控条件，但整包编译+刷机周期太长。
- 现场已经能 SSH 到主猫，且允许替换 `/usr/sbin/fttrd`（必须先备份）。
- 改动仅限 userspace；涉及内核驱动、设备树、FPGA 镜像时必须走正式构建。

## 工具链

目标机是 aarch64 + musl，用 glibc 静态交叉构建即可直接运行，不必引入 zig。

1. 交叉 C 编译器（供 ring 等 C 依赖与链接使用）：

   ```sh
   apt-get update && apt-get install -y curl gcc-aarch64-linux-gnu
   ```

2. Rust：官方静态源下载慢时换镜像（两条都要设）：

   ```sh
   RUSTUP_DIST_SERVER=https://rsproxy.cn \
   RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup \
   ./rustup-init -y --default-toolchain stable --profile minimal
   rustup target add aarch64-unknown-linux-gnu
   ```

3. `~/.cargo/config.toml`：

   ```toml
   [source.crates-io]
   replace-with = "sparse-mirror"

   [source.sparse-mirror]
   registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"

   [net]
   git-fetch-with-cli = false

   [target.aarch64-unknown-linux-gnu]
   linker = "aarch64-linux-gnu-gcc"
   rustflags = ["-C", "target-feature=+crt-static"]
   ```

   现场机器可能没有 `git`；`git-fetch-with-cli = false` 让 cargo 用自带 libgit2 拉 git 依赖。
   注意发行版自带 `rustc` 可能过旧（`Option::is_none_or` 需要 1.82+）。

## 构建

1. 先在本机跑单测，并把**既有失败**记录在案（例如需要非 root 才能满足的权限断言），不要算到本轮改动上：

   ```sh
   cargo test --no-default-features --features all-devices,acs,mqtt --lib
   ```

2. 交叉构建，feature 必须与现场一致（缺 `acs`/`mqtt` 会静默改变服务行为）：

   ```sh
   CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
   cargo build --release --locked --no-default-features \
     --features all-devices,acs,mqtt \
     --target aarch64-unknown-linux-gnu --bin fttrd
   ```

3. 校验产物，不要部署到一半才发现是旧二进制：

   ```sh
   file target/aarch64-unknown-linux-gnu/release/fttrd   # 期望：ARM aarch64, static-pie linked, stripped
   strings -a target/aarch64-unknown-linux-gnu/release/fttrd | grep -m1 '<新增日志标记>'
   md5sum target/aarch64-unknown-linux-gnu/release/fttrd
   ```

4. 首次可先在设备上直接执行一次：它会因"光硬件已被现有 fttrd 占用"而退出，这恰好证明二进制能运行、动态加载正常。

## 部署与回滚

```sh
# 传输（现场不一定有 scp 认证，用 stdin 即可）
ssh root@<OLT_HOST> 'cat > /tmp/fttrd.new && chmod 755 /tmp/fttrd.new && md5sum /tmp/fttrd.new' < fttrd

# 备份 + 安装 + 重启
ssh root@<OLT_HOST> 'cp -a /usr/sbin/fttrd /root/fttrd.orig && cp /tmp/fttrd.new /usr/sbin/fttrd && /etc/init.d/fttr restart'

# 回滚
ssh root@<OLT_HOST> 'cp /root/fttrd.orig /usr/sbin/fttrd && /etc/init.d/fttr restart'
```

- 重启会重新发现、测距并重配**所有** ONU，ONU ID 可能变化；实验记录必须标注这一点。
- 部署后按"启动日志 → profile 回读 → 数据面"顺序看，先确认进程起来了再谈业务。
- 保留 `/root/fttrd.orig` 直到本轮交接完成，并在记录里写明回滚方式。

## 边界与风险

- 这不是交付固件；不要用临时二进制替代正式发布产物，也不要把它写进镜像说明。
- 覆盖系统二进制前必须备份；不要在设备上盲跑全套单测或并发跑多个诊断进程（它们会争抢同一把 ONU 锁与硬件租约）。
- 目标机若有自定义服务（MQTT、ACS、管理网），构建时缺省 feature 会改变现场行为，必须提前说明并验证。
- 部署后出现的"新失败点"同样要按[定位参考](omci-failure-triage.md)分层，不能因为换了二进制就归因于新代码。
