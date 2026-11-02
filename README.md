# OpenWrt FTTR

H3C HM2004-DU 下联 Micro-OLT 的 OpenWrt feed。源码仓库：
https://github.com/LazuliKao/openwrt-fttr 。

## 目录

- `h3c-fmcs/`：内核驱动、FPGA 初始化、PLOAM/OMCI/GEM 通道及驱动测试。
- `h3c-fttr-tools/`：Rust 控制器 `fttrd`、命令入口 `fttrctl`、设备适配和可裁剪 ACS。

## 接入 PonWrt

在 OpenWrt 的 `feeds.conf.default` 或自定义 `feeds.conf` 中加入：

```text
src-git fttr https://github.com/LazuliKao/openwrt-fttr.git
```

在 PonWrt 根目录运行：

```sh
./scripts/feeds update -a
# 已有构建树先移除旧 feed 的两个包链接；不删除其他 PON 包。
./scripts/feeds uninstall h3c-fmcs h3c-fttr-tools
./scripts/feeds install -p fttr h3c-fmcs h3c-fttr-tools
./scripts/feeds install -a
make defconfig
make package/feeds/fttr/h3c-fmcs/compile V=s -j2
make package/feeds/fttr/h3c-fttr-tools/compile V=s -j2
```

## 编译选项

| OpenWrt 配置 | Cargo feature | OpenWrt 默认 | Cargo 默认 |
| --- | --- | --- | --- |
| `CONFIG_FTTR_DEVICE_HL202` | `device-hl202` | 开 | 开 |
| `CONFIG_FTTR_DEVICE_HWTC` | `device-hwtc` | 开 | 关 |
| `CONFIG_FTTR_DEVICE_STAR` | `device-star` | 开 | 关 |
| `CONFIG_FTTR_ACS` | `acs` | 开 | 关 |
| `CONFIG_FTTR_MQTT` | `mqtt` | 开 | 开 |
| `CONFIG_FTTR_NPU` | `npu` | 开 | 开 |

OpenWrt 使用 `--no-default-features`，逐项显式映射上述选项。设备及 ACS
feature 不会隐式开启 MQTT；`all-devices` 组合上述全部设备模块。Linux 下在
`h3c-fttr-tools/src` 验证全部目标：

```sh
cargo check --locked --all-targets --no-default-features --features all-devices,acs,mqtt,npu
cargo check --locked --all-targets --no-default-features --features all-devices,acs,mqtt
cargo check --locked --all-targets --no-default-features --features all-devices,acs
```


## Windows Rust 交叉编译

在 `h3c-fttr-tools/src` 中使用已安装的 Rust target、Zig 和 cargo-zigbuild：

```powershell
$previous = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=-crt-static -C link-arg=-dynamic'
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl --no-default-features --features all-devices,acs,mqtt,npu --bin fttrd --bin fttrctl
} finally {
    $env:RUSTFLAGS = $previous
}
```

产物位于 `src/target/aarch64-unknown-linux-musl/release/`。
内核驱动与固件镜像使用 Linux OpenWrt 构建环境。

## LuCI SDK 生成

`fttr-api` 共享后端声明并生成 LuCI 类型和客户端，默认输出到
`../luci-app-fttr/src/api/generated/`。在仓库根目录执行：

```sh
cargo run --locked --manifest-path h3c-fttr-tools/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --generate
cargo run --locked --manifest-path h3c-fttr-tools/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --check
```

其他布局追加 `--out-dir FRONTEND/src/api/generated`。`--check` 只检查生成
一致性，不写文件。更新后在前端运行 `pnpm typecheck`、`pnpm test`。

CI 同时检查前端生成文件和 ACL；可用 `FTTR_FRONTEND_REF` 仓库变量或手动运行时的 ref 选择配套前端版本。
