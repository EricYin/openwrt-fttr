# OpenWrt FTTR

包含 H3C HM2004-DU Micro-OLT 平台组件及多厂商 FTTR 用户态控制器的 OpenWrt feed。源码仓库：
https://github.com/LazuliKao/openwrt-fttr 。

## 目录

- `h3c-fmcs/`：内核驱动、FPGA 初始化、PLOAM/OMCI/GEM 通道及驱动测试。
- `fttr/`：Rust FTTR 控制器 `fttrd`、命令入口 `fttrctl`、多厂商设备适配和可裁剪 ACS。

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

`fttr/src/Cargo.toml` 是 workspace 根，所有内部 crate 共用根目录
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

## HM2004-DU 光模块校准与温补

`fttrd` 的光硬件 owner 统一应用 UX3326 的寄存器镜像、遥测系数和温度表。
在 LuCI 校准页分别执行 **上传保存 → 保存来源选择 → 应用所选校准**；上传文件本身不切换来源。
配置保存在 `/etc/config/fttr`：

```uci
config calibration 'calibration'
    option source 'auto'
```

`auto`（缺省）按有效 Factory → firmware 文件 → builtin 选择；也可显式选择
`factory`、`firmware`、`builtin`。显式来源不可用或无效时返回错误，不自动回退。
firmware 文件路径为 `/lib/firmware/modified_UX3326.bin`，由现有 sysupgrade keep 文件保留。
来源配置与业务同步 revision 分离，改来源不会触发 ONU 业务重配。

```sh
ubus call fttr get_calibration '{}'
ubus call fttr select_calibration '{"source":"firmware"}'
ubus call fttr apply_calibration '{}'
```

`apply_calibration` 返回异步 job，需按任务 API 查询最终结果。应用会通过 owner 暂停发现和业务、
重新初始化并核对硬件读回；成功后同时发布该版本的遥测系数和温补表。关光期间应用保持关光，
显示 pending，下一次启用才加载。`get_calibration.state` 区分 selected、candidate、active、
hardware_applied 和 pending，并报告原始/有效 SHA-256；active 保留上次成功版本历史，
仅当 hardware_applied 为真时才表示当前已应用。手动关闭再开启也会重新选择候选版本。

完整文件为 288 字节；兼容 56 字节寄存器镜像，但其遥测系数由内置历史样本补全，
**不启用温补**，原始与有效哈希分别报告。完整表中 `Mod_Lut_EN=1` 时，owner 在启用/
应用后首次检查，随后约每 60 秒用主温度系数计算 84 项表索引，必要时依次写 BOSA
`0x06/0x07` 并验证。温补独立于 GPON MAC TX 开关；owner 停止、复位或关光时停止写入。
ADC 失败或温度索引越界跳过该次，部分写入或验证失败则暂停发现并尝试复位，需显式恢复。
状态提供最近检查/写入、原始温度、索引、目标码及错误。

遥测使用浮点商而非先整除；无效分母、溢出、非正 TX 功率返回不可用，不替换默认分母。
`fttrctl bosa-diag` 从运行中的 daemon 获取同版本遥测；低层写寄存器命令及其别名遵守 owner 锁。
RESET_N 读回、MAC TX 和实际光输出是不同证据；温补码没有已确认的 dBm 目标换算。
本轮实现不包含原厂 ALS/LOS 自动关光状态机。离线调查和验收边界见本地
`EVIDENCE/HM2004-DU-STOCK-TX-CONTROL.md`、`EVIDENCE/HM2004-DU-OPTICAL-IMPLEMENTATION.md`。

## Windows Rust 交叉编译

在 `fttr/src` 中使用已安装的 Rust target、Zig 和 cargo-zigbuild：

```powershell
$previous = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=-crt-static -C link-arg=-dynamic'
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl --no-default-features --features platform-h3c-hm2004-du,all-devices,acs,mqtt,npu,allocator-mimalloc --bin fttrd --bin fttrctl
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
cargo run --locked --manifest-path fttr/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --generate
cargo run --locked --manifest-path fttr/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --check
```

其他布局追加 `--out-dir FRONTEND/src/api/generated`。`--check` 只检查生成
一致性，不写文件。完整链路在前端运行 `pnpm sdk:generate`，自动生成
TypeScript 类型、客户端、ACL 和 Zod mini 校验；`pnpm check` 依次检查生成一致性。
