# OpenWrt FTTR

包含 H3C HM2004-DU Micro-OLT 平台组件及多厂商 FTTR 用户态控制器的 OpenWrt feed。  
源码仓库：<https://github.com/LazuliKao/openwrt-fttr>

## 仓库结构

- `h3c-fmcs/`：内核驱动、FPGA 初始化、PLOAM/OMCI/GEM 通道及驱动测试。
- `h3c-fmcs-firmware/`：H3C HM2004-DU Micro-OLT FPGA 固件包（`FTTR_TOP.sbit`）。
- `fttr/`：Rust FTTR 控制器 `fttrd`、CLI 工具 `fttrctl`、多厂商设备适配与可裁剪 ACS（用户态源码位于 `fttr/src/src/`）。

---

## 快速接入（PonWrt）

### 1. 添加 Feed 并编译

在 OpenWrt 的 `feeds.conf.default` 或自定义 `feeds.conf` 中加入：

```text
src-git fttr https://github.com/LazuliKao/openwrt-fttr.git
```

在 PonWrt 根目录执行：

```sh
./scripts/feeds update fttr
./scripts/feeds install -a -p fttr
make defconfig
make package/feeds/fttr/h3c-fmcs/compile V=s
make package/feeds/fttr/openwrt-fttr/compile V=s
```

### 2. LuCI 管理界面（独立仓库）

LuCI 包由 [luci-app-fttr](https://github.com/LazuliKao/luci-app-fttr) 独立维护，OpenWrt 包位于其 `package` 分支，可直接作为 feed（`src-git ...;package`）或克隆到 `package/` 下编译：

```sh
git clone --single-branch --branch package https://github.com/LazuliKao/luci-app-fttr.git package/luci-app-fttr
make defconfig
make package/luci-app-fttr/compile V=s
```

---

## 运行时配置（`/etc/config/fttr`）

### 1. 三层设备适配

每个 `config device` 需显式指定三个适配器（可用候选项通过 `fttrctl capabilities --json` 查看）：

| 层 | UCI 字段 | 职责 | 示例 |
| --- | --- | --- | --- |
| A | `omci_adapter` | 光侧业务配置 | `generic`、`hl202`、`hwtc` |
| B | `bootstrap_adapter` | 管理引导策略 | `none`、`manual-acs`、`omci-acs`、`hl202-mqtt-acs` |
| C | `provisioning_adapter` | 业务传输与参数契约 | `none`、`hl202-mqtt`、`hl202-cu`、`hwtc-hw`、`vendor-cu-candidate`、`vendor-ct-candidate` |

示例（HL202 通过 MQTT 下发 ACS 地址，再使用联通参数契约配置业务）：

```uci
config device 'child'
	option optical_sn 'H3CT1234ABCD'
	option omci_adapter 'hl202'
	option bootstrap_adapter 'hl202-mqtt-acs'
	option provisioning_adapter 'hl202-cu'
```

- 三层独立校验，不自动回退或切换；公共配置无需填写 `cwmp_id`。
- 切换 `provisioning_adapter` 会使旧业务操作失效并要求重新发现；切换 A/B 保留业务历史。
- 旧配置项 `adapter`、`provisioning_method`、`cwmp_contract`、`omci_management` 已不再使用。

### 2. ONU OMCI 凭据认证

默认全局策略为 `disabled`（不校验凭据，仍尝试读取上报值用于展示）。需要开启认证时：

```uci
config omci_auth 'global'
	option schema_version '1'
	option auth_mode 'loid_password'
	option auth_loid '<EXPECTED_LOID>'
	option auth_password '<PASSWORD>'
```

- 设备级 `auth_mode` 支持 `inherit`（默认）、`disabled`、`loid_password`；设备覆盖时须同时提供本地 `auth_loid` 与 `auth_password`。
- 标准后端使用 ONU-G（ME 256，LOID 1–24 字节、密码 1–12 字节可打印 ASCII，保留空白与大小写）；HL202 使用 ME 65530 私有布局（各限 10 字节）。
- 业务配置、管理引导、ACS 写任务与 carrier 恢复均受认证门控；H3C 平台需配套本仓库驱动。

### 3. HM2004-DU 光模块校准与温补

可在 LuCI 校准页执行 **上传保存 → 保存来源选择 → 应用所选校准**，或通过 UCI / `ubus` 操作：

```uci
config calibration 'calibration'
	option source 'auto' # auto | factory | firmware | builtin
```

```sh
ubus call fttr get_calibration '{}'
ubus call fttr select_calibration '{"source":"firmware"}'
ubus call fttr apply_calibration '{}'
```

- `firmware` 路径为 `/lib/firmware/modified_UX3326.bin`（完整 288 字节启用温补；兼容 56 字节寄存器镜像但不启用温补）。
- 校准来源与业务同步版本分离，修改来源不会触发 ONU 业务重配。
- `fttrctl bosa-diag` 可从运行中的 `fttrd` 获取当前遥测与温补状态。

---

## 编译选项与开发指南

### OpenWrt 编译配置与 Cargo Features

<details>
<summary>展开查看网关平台与下游 ONU 编译开关对照表</summary>

#### 网关 / OLT 平台

| OpenWrt 配置 | Cargo feature | OpenWrt 默认 | Cargo 默认 | 状态 |
| --- | --- | --- | --- | --- |
| `CONFIG_FTTR_PLATFORM_H3C_HM2004_DU` | `platform-h3c-hm2004-du` | ✅ | ✅ | 依赖 H3C FMCS 驱动 |
| `CONFIG_FTTR_PLATFORM_FIBERHOME_AN4475_G2` | `platform-fiberhome-an4475-g2` | ❌ | ❌ | 编译期 stub；板级身份及驱动尚未调查 |
| `CONFIG_FTTR_PLATFORM_TIANYI_TYFZA1` | `platform-tianyi-tyfza1` | ❌ | ❌ | 编译期 stub；板级身份及驱动尚未调查 |
| — | `all-platforms` | — | ❌ | Cargo 聚合 feature |

#### 下游 ONU 适配与服务

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

</details>

### Workspace 测试与诊断工具

`fttr/src/Cargo.toml` 为 workspace 根，默认构建 `fttrd` 和 `fttrctl`。在 `fttr/src` 目录执行主机测试：

```sh
cargo test --locked --workspace --features fttr-api/codegen
cargo test --locked --lib --no-default-features --features platform-h3c-hm2004-du,all-devices,acs,mqtt,npu
cargo check --locked --all-targets --no-default-features --features all-platforms,all-devices,acs,mqtt,npu,diagnostics
```

诊断工具（`fttr-platform-probe`、`fttr-omci-mib`、`fttr-omci-probe`、`fttr-omci-service`、`fttr-ranging` 等）需显式启用 `diagnostics`；`fttr-k155-service` 还需 `device-hwtc`，`fttr-star-observe` 还需 `device-star`：

```sh
cargo build --locked --features diagnostics,device-star --bin fttr-star-observe
fttr-star-observe 02:00:00:00:00:01
```

> CLI 受支持命令以 `fttrctl help` 为准；运行独立 `fttr-ranging` 前需先停止 `fttrd`。

### Windows Rust 交叉编译

在 `fttr/src` 中使用已安装的 Rust target、Zig 和 `cargo-zigbuild`（内核驱动与完整固件仍需 Linux OpenWrt 环境）：

```powershell
$previous = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=-crt-static -C link-arg=-dynamic'
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl --no-default-features --features platform-h3c-hm2004-du,all-devices,acs,mqtt,npu,allocator-mimalloc --bin fttrd --bin fttrctl
} finally {
    $env:RUSTFLAGS = $previous
}
```

产物位于 `fttr/src/target/aarch64-unknown-linux-musl/release/`。

### LuCI SDK 生成

在仓库根目录执行 `fttr-sdk` 生成或校验前端接口定义（通过 `--out-dir` 指定目标路径）：

```sh
cargo run --locked --manifest-path fttr/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --generate --out-dir <OUT_DIR>
cargo run --locked --manifest-path fttr/src/crates/fttr-api/Cargo.toml --features codegen --bin fttr-sdk -- --check --out-dir <OUT_DIR>
```
