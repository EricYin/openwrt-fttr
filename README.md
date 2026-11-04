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
| `CONFIG_FTTR_DEVICE_ZTE` | `device-zte` | 开 | 关 |
| `CONFIG_FTTR_DEVICE_SCTY` | `device-scty` | 开 | 关 |
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

SCTY 仅精确匹配 `COMHEART TY-FS803G`，复用按 MIB 发现资源的标准 OMCI
桥接流程。当前为桥接适配入口；双向 GEM、管理网络与 CWMP/Wi-Fi 尚待独立
现场验收，不包含推测的 CWMP 串号规则或私有参数路径。

### Workspace 与诊断工具

`h3c-fttr-tools/src/Cargo.toml` 是 workspace 根，所有内部 crate 共用根目录
`Cargo.lock`。普通 `cargo build` 只构建主包，默认二进制为 `fttrd` 和
`fttrctl`；OpenWrt 打包也显式限定这两个入口。完整主机测试在该目录执行：

```sh
cargo test --locked --workspace --features fttr-api/codegen
cargo test --locked --lib --no-default-features --features all-devices,acs,mqtt,npu
cargo check --locked --all-targets --no-default-features --features all-devices,acs,mqtt,npu,diagnostics
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

### 配置热加载与同步

保存并应用后，后端校验并接收最新配置；空闲时立即加载，有同步任务时自动等待
安全切换点，无需再次点击重载。连续应用只保留最新版本，等待期间不再启动旧版本
的新任务。未执行的旧任务直接让位；已执行的事务完成后，在安全项目边界跳过
剩余旧项目，保留既有回读结果和资源所有权。最后一个阻塞解除后立即调度新版本，
不等待周期扫描或旧重试退避。

`get_sync` / `config_reload` 的 `config_load` 区分 `waiting` 与 `applied`，包含待加载
版本、请求时间及阻塞操作。`revision` 仍表示当前已加载版本；配置已加载不等于
所有设备已同步。离线设备恢复后再按当前版本同步。手动模式仍需显式应用差异，
结果未知的写入仍需先核实。非法的新请求取消旧的待加载版本并返回校验错误；
服务重启直接读取持久化配置。

### 光链路控制

LuCI **状态页面最上方**集中提供 FPGA 总开关、GPON 下行发送、下行 FEC 三项设置，
同时显示保存值、硬件读回、应用错误和各 ONU 的 OMCI FEC PM 状态。
三项配置仅属于 `config optical 'optical'`：

| optical 选项 | 默认 | 含义 |
| --- | --- | --- |
| `enabled` | `1` | FPGA 总开关，控制现有光输出复位与发现流程，并非板卡电源 |
| `downstream_tx_enabled` | `1` | GPON 下行发送，`0x01000004` Bit 1 |
| `fec_enabled` | `0` | 下行纠错，`0x01000004` Bit 3，同时启用 OMCI PM 自动准备、统计与诊断 |

不读取或迁移 provisioning 中的旧 FEC/下行字段；旧字段会被配置校验拒绝。
`optical_status` 回读整组保存配置及硬件状态，`optical_set` 要求传入全部三个布尔值。
保存使用独立 UCI 快照和原子替换，保留其他设置，不消费共享的未提交编辑。
同一 PLOAM 硬件所有者按顺序持久化和应用请求；这些配置不进入 WAN/Wi-Fi 同步计划、
不改变业务配置版本，也不等待正在运行的业务同步操作。重启与服务 reload 都恢复保存值。

关闭总开关或下行发送会停止发现、取消 ONU 服务、降低 carrier 并标记离线；重新开启后
重新注册。总开关关闭时实际发送与 FEC 均关闭，但保留两项子开关的保存值。
只切换 FEC 不会主动重新注册 ONU。寄存器读改写保留其他位并读回校验；硬件置位按开启
下行纠错处理，ONU 的 OMCI PM 不可用单独显示，不改变硬件开启状态。不控制上行 FEC。

### FEC 统计用法

```sh
uci set fttr.optical.enabled='1'
uci set fttr.optical.downstream_tx_enabled='1'
uci set fttr.optical.fec_enabled='1'
uci commit fttr
fttrctl optical reload --json
```

```sh
fttrctl status --json          # sub_gateways[].fec：当前代次的状态与证据
fttrctl fec 0 --json           # 对当前 ONU 0 串行执行标准下行 PM GET
fttrctl fec 0 --create-pm       # 手动重试创建缺失的 class 312 PM 实例
```

先用状态确认当前 ONU ID；CLI 将请求绑定到串号、注册代次和已发现的 ANI-G。
FEC 开启后，当前和新注册 ONU 由各自串行 OMCI worker 读取实际 ANI-G 及 class 312 PM。
标准 ANI-G 定义没有通用的可写 FEC 总开关，不伪造能力属性或写入只读属性。
实例存在时只读；仅在 GET 返回 unknown instance（结果 5）时自动 CREATE，阈值指针为 0，
随后 GET 验证并读取计数。单次请求超时 2 秒，准备与采集总预算 8 秒；失败按 ONU
退避 5 分钟，不作为活体失败。切换 FEC 会刷新配置代次，及时唤起采集，
取消旧代次排队请求，并隐藏旧统计；关闭后不再发送后续 FEC 请求。
`--create-pm` 使用相同的 unknown-instance 限制；
不重置已有 PM，不改既有阈值，不自动删除创建的 PM。返回 `created_pm` 标明是否创建，
后续正常遥测会读取它。若设备拒绝该语义，保留原始结果码，不按成功处理。

class 312 的 GET 是**最近完成的 15 分钟下行 PM 区间**，不是实时、累计或上行计数；
区间结束前可能没有有效的新结果。实例缺失、拒绝或超时不等于 ONU 不支持 FEC。
`control_supported=true` 表示提供下行板级控制；`downstream_enabled` 来自所有者最近一次
寄存器读回，未初始化或应用失败时为 `null`。`upstream_enabled` 仍为 `null`；
计数为零不推断开关关闭。

正常保留配置的 sysupgrade 会同时备份 `/lib/firmware/modified_UX3326.bin`
校准文件（若存在），保留项由 `/lib/upgrade/keep.d/fttr` 声明。

持久化状态按内容去重：管理网络 journal 在成功提交后缓存快照，相同状态不再
写盘；共用原子写入层也避免重写相同文件。事务状态变更仍同步落盘，失败重试
保留文件和目录同步屏障，以保证断电恢复。


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
一致性，不写文件。完整链路在前端运行 `pnpm sdk:generate`，自动生成
TypeScript 类型、客户端、ACL 和 Zod mini 校验；`pnpm check` 依次检查生成一致性。
