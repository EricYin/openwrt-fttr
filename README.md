# OpenWrt FTTR

包含 H3C HM2004-DU Micro-OLT 平台组件及多厂商 FTTR 用户态控制器的 OpenWrt feed。源码仓库：
https://github.com/LazuliKao/openwrt-fttr 。

## 目录

- `h3c-fmcs/`：内核驱动、FPGA 初始化、PLOAM/OMCI/GEM 通道及驱动测试。
- `fttr-tools/`：Rust FTTR 控制器 `fttrd`、命令入口 `fttrctl`、多厂商设备适配和可裁剪 ACS。

## 接入 PonWrt

在 OpenWrt 的 `feeds.conf.default` 或自定义 `feeds.conf` 中加入：

```text
src-git fttr https://github.com/LazuliKao/openwrt-fttr.git
```

在 PonWrt 根目录运行：

```sh
./scripts/feeds update -a
# 已有构建树先移除旧用户态包链接；不删除其他 PON 包。
# 设备上的 ipk 包改名升级流程需先移除旧 h3c-fttr-tools 包，再安装 openwrt-fttr。
./scripts/feeds uninstall h3c-fmcs h3c-fttr-tools openwrt-fttr
./scripts/feeds install -p fttr h3c-fmcs openwrt-fttr
./scripts/feeds install -a
make defconfig
make package/feeds/fttr/h3c-fmcs/compile V=s -j2
make package/feeds/fttr/openwrt-fttr/compile V=s -j2
```

### LuCI 管理界面

LuCI 包由 [luci-app-fttr](https://github.com/LazuliKao/luci-app-fttr) 独立维护。
该仓库的 `package` 分支根目录包含完整 Makefile、构建后的 JS、菜单、ACL 和翻译，
本 feed 不再保存 LuCI 包或同步 nightly 哈希。

确认前端 CI 已发布 `package` 分支后，在构建树中执行：

```sh
# 旧构建树先卸载旧 feed 的 LuCI 包链接，再导入独立包。
./scripts/feeds uninstall luci-app-fttr
git clone --single-branch --branch package https://github.com/LazuliKao/luci-app-fttr.git package/luci-app-fttr
make defconfig
make package/luci-app-fttr/compile V=s -j2
```

按需选择独立的 `luci-i18n-fttr-zh-cn` 翻译包。可复现构建应固定 `package`
分支上的提交。前端开发与发布方法见前端仓库 README。

## 编译选项

### 网关 / OLT 平台

| OpenWrt 配置 | Cargo feature | OpenWrt 默认 | Cargo 默认 | 状态 |
| --- | --- | --- | --- | --- |
| `CONFIG_FTTR_PLATFORM_H3C_HM2004_DU` | `platform-h3c-hm2004-du` | ✅ | ✅ | 依赖 H3C FMCS 驱动 |
| `CONFIG_FTTR_PLATFORM_FIBERHOME_AN4475_G2` | `platform-fiberhome-an4475-g2` | ❌ | ❌ | 编译期 stub；板级身份及驱动尚未调查 |
| `CONFIG_FTTR_PLATFORM_TIANYI_TYFZA1` | `platform-tianyi-tyfza1` | ❌ | ❌ | 编译期 stub；板级身份及驱动尚未调查 |
| — | `all-platforms` | — | ❌ | Cargo 聚合 feature；OpenWrt 用以上独立开关支持任意子集 |


### 下游 ONU 适配与服务

| OpenWrt 配置 | Cargo feature | OpenWrt 默认 | Cargo 默认 |
| --- | --- | --- | --- |
| `CONFIG_FTTR_DEVICE_HL202` | `device-hl202` | ✅ | ✅ |
| `CONFIG_FTTR_DEVICE_HWTC` | `device-hwtc` | ✅ | ❌ |
| `CONFIG_FTTR_DEVICE_STAR` | `device-star` | ✅ | ❌ |
| `CONFIG_FTTR_DEVICE_ZTE` | `device-zte` | ✅ | ❌ |
| `CONFIG_FTTR_DEVICE_SCTY` | `device-scty` | ✅ | ❌ |
| `CONFIG_FTTR_DEVICE_FIBERHOME` | `device-fiberhome` | ✅ | ❌ |
| `CONFIG_FTTR_DEVICE_REALTEK` | `device-realtek` | ✅ | ❌ |
| `CONFIG_FTTR_ACS` | `acs` | ✅ | ❌ |
| `CONFIG_FTTR_MQTT` | `mqtt` | ✅ | ✅ |
| `CONFIG_FTTR_NPU` | `npu` | ✅ | ✅ |
| `CONFIG_FTTR_MIMALLOC` | `allocator-mimalloc` | ✅ | ❌ |

### Workspace 与诊断工具

`fttr-tools/src/Cargo.toml` 是 workspace 根，所有内部 crate 共用根目录
`Cargo.lock`。普通 `cargo build` 只构建主包，默认二进制为 `fttrd` 和
`fttrctl`；OpenWrt 打包也显式限定这两个入口。完整主机测试在该目录执行：

```sh
cargo test --locked --workspace --features fttr-api/codegen
cargo test --locked --lib --no-default-features --features platform-h3c-hm2004-du,all-devices,acs,mqtt,npu
cargo check --locked --all-targets --no-default-features --features all-platforms,all-devices,acs,mqtt,npu,diagnostics
```

`fttr-platform-probe`、`fttr-omci-mib`、`fttr-omci-probe`、`fttr-omci-service`、
`fttr-ranging` 需显式启用 `diagnostics`；`fttr-k155-service` 还需 `device-hwtc`，
`fttr-star-observe` 还需 `device-star`。例如：

```sh
cargo build --locked --features diagnostics,device-star --bin fttr-star-observe
fttr-star-observe 02:00:00:00:00:01
```

STAR 观察工具要求传入源 MAC；不再内置现场设备地址。独立 ranging 工具仍需先停止
`fttrd`，OMCI MIB 入口复用控制器 inventory 核心。CLI 的受支持命令以
`fttrctl help` 为准，手动 `discover/register/auth/sync-wifi/provision` 入口已移除。

### miniOLT 只读 FPGA 诊断

`fttrctl fpga-diag [ONU_ID|all] [--samples N] [--interval-ms N] [--json]`
通过 daemon 读取原厂 `moltdbg` 已确认的授权配置、LoB/BIP 和全局错误计数；
`--offline` 显式直接读取。JSON 为逐行快照及模差，跨注册代次不计算 ONU 差值。
计数可能回绕、清零或饱和；寄存器不是原子快照，不能单凭累计值判定故障。
字段来源、地址与解释边界见本地 `EVIDENCE/moltdbg-readonly-diagnostics.md`。

## Windows Rust 交叉编译

在 `fttr-tools/src` 中使用已安装的 Rust target、Zig 和 cargo-zigbuild：

```powershell
$previous = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=-crt-static -C link-arg=-dynamic'
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl --no-default-features --features platform-h3c-hm2004-du,all-devices,acs,mqtt,npu --bin fttrd --bin fttrctl
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
cargo run --locked --manifest-path fttr-tools/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --generate
cargo run --locked --manifest-path fttr-tools/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --check
```

其他布局追加 `--out-dir FRONTEND/src/api/generated`。`--check` 只检查生成
一致性，不写文件。完整链路在前端运行 `pnpm sdk:generate`，自动生成
TypeScript 类型、客户端、ACL 和 Zod mini 校验；`pnpm check` 依次检查生成一致性。
