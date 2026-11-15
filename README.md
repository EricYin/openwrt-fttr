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

---

## 开源协议

- `fttr/` 及仓库根目录通用内容采用 [Apache-2.0](LICENSE)（见 [fttr/LICENSE](fttr/LICENSE)）。
- `h3c-fmcs/` 内核驱动与配套测试代码采用 [GPL-2.0-only](h3c-fmcs/LICENSE)。
