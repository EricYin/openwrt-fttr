# 能力发现与适配决策

本参考按 2026-10-25 工作区核对，"通用 profile 的最小充分集"一节按 2026-10-28 现场适配补充；2026-11-01 补充厂商/型号分层和管理身份发现。执行时重读当前源码；这里的源码行为不代表现场已部署。
下列路径均相对仓库根目录。

## 当前源码地图

| 职责 | 入口 |
| --- | --- |
| 项目约定与构建 | `AGENTS.md`、`README.md`、`EVIDENCE/ENV.md` |
| 设备选择、feature 裁剪 | `h3c-fttr-tools/src/src/devices/mod.rs`、`h3c-fttr-tools/src/Cargo.toml` |
| 设备代码分层约定 | `h3c-fttr-tools/src/src/devices/AGENTS.md` |
| STAR 厂商入口、SG631Z-C 身份规则 | `h3c-fttr-tools/src/src/devices/star/mod.rs`、`h3c-fttr-tools/src/src/devices/star/sg631/mod.rs` |
| 通用发现与配置 | `h3c-fttr-tools/src/src/devices/generic/inventory.rs`、`h3c-fttr-tools/src/src/devices/generic/service.rs` |
| 业务对象与管理关联 | `h3c-fttr-tools/src/src/controller/business.rs`、`h3c-fttr-tools/src/src/controller/management.rs` |
| 注册、调度和健康 | `h3c-fttr-tools/src/src/controller/runtime.rs`、`h3c-fttr-tools/src/src/controller/service.rs`、`h3c-fttr-tools/src/src/controller/health.rs` |
| MIB、传输和资源租约 | `h3c-fttr-tools/src/src/pon_engine/omci/mib.rs`、`h3c-fttr-tools/src/src/pon_engine/omci/platform/fmcs.rs`、`h3c-fttr-tools/src/src/pon_engine/omci/platform/leases.rs` |
| CWMP 命令边界 | `h3c-fttr-tools/src/src/acs_cli.rs` |
| 内核收发与通道映射 | `h3c-fmcs/src/fmcs_netdev.c`、`h3c-fmcs/src/fmcs_main.c` |
| 持久化服务与打包 | `h3c-fttr-tools/files/fttr.init`、`h3c-fttr-tools/files/fttr.config`、`h3c-fttr-tools/Makefile` |

## 发现顺序

1. 获取当前状态并以私有身份匹配目标；记录采集时间、ONU ID、注册代次和运行版本。共享输出使用别名。
2. 核对当前 CLI 帮助，再采集完整 MIB 与必要的表。保留完整性、掩码、原始/解码覆盖情况；输出前过滤凭据。
3. 建立资源关系：T-CONT 262 → GEM CTP 268 → GEM IW 266 → 桥 45/47 → UNI 11 或 VEIP 329；按需补充 GAL 272、过滤 84、EVTO 171、队列 277、调度器 278、管理 340/157。
4. 检查指针、资源归属、管理/运行状态。状态字和字节值按当前 schema/规范解释，不跨 ME 猜测极性。
5. 对未知实例错误定位到具体操作、类、实例和掩码；不能因为掩码为零就改为“属性不支持”。超时也不能解释为实体不存在。
6. MIB 未解码时先确认原始记录，不把空属性列表解释为没有配置。以完整性和冲突状态决定能否自动规划。

## 适配选择

- **标准实体布局差异：**改善通用发现，使用现场实际实例；无可用资源时明确失败，不虚构 UNI 或抢占他人 T-CONT。
- **标准流程/参数差异：**用最小受控实验建立证据，再引入明确策略。检查旧实例排序、固定上联端口号、重试及部分配置恢复的兼容性。
- **厂商特有行为：**确认型号/版本及复现条件后按 `devices/AGENTS.md` 新增 `devices/<vendor>/<model>/`，由厂商入口分派，并核对厂商级 Cargo feature 和 OpenWrt 映射。通用桥接可以复用，专属身份或参数规则不能放进 `generic/`。
- **WAN/ACS 配置未知：**先建立管理入口和读回能力；不能用重置、盲扫 VLAN 或硬编码网上默认值代替发现。

选择逻辑的边界：**厂商码相同不代表拓扑相同**。只对已验证型号保留固定拓扑的厂商 profile，其余型号应落到通用 profile；否则一个新型号就会以"固定实例不存在"的形式失败（见[HWTC 案例](hwtc-hg8010c-spgmac-case.md)）。

## 通用 profile 的最小充分集

通用 profile 的正确性来自"从 MIB 读"，不是"模板写得更全"。当前实现要求的要素：

1. **完整 MIB 上传**后再规划，任何缺记录都不能当成"没有配置"。
2. **T-CONT**：优先复用已分配给我们 Alloc-ID 的实例；其它 OLT 留下的分配可复用；只把本机槽位段（`0x100..0x10f`）当作可能属于其它业务而排除。Alloc-ID 写不进去时降级为跳过并记录，GEM 仍指向已发现的 T-CONT。
3. **端点**：UNI(11) 与 VEIP(329) 全量发现并保持设备自身顺序；不要为了"顺序好看"重排实例映射。
4. **桥端口复用**：端点已有 ME 47（TpPointer 命中且 BridgeId 与所选桥一致）就复用，不要给同一 UNI 再建一个——会撞 PortNum，并覆盖厂商自己的管理 VLAN/EVTO。
5. **PortNum 分配**：新建端口从 MIB 中未占用的号码里取；ANI 端口的 PortNum 要复用本槽位已有值，保证重配幂等。
6. **EVTO 复用**：按 AssociatedMePointer 匹配端点已有实例；`DownstreamMode` 与透明表属于增强项，写入失败只记录，不放弃整条 profile。
7. **GEM CTP**：属性 4（上行 TM 指针）必须指向本 GEM 的 T-CONT；这是多家实现都会校验的参数。
8. **实体编号与槽位解耦**：ONT 的 MIB 是持久的，ONU ID 会变。发现 ANI 绑定指向其它槽位的 GEM 时先退役（删 47/266/268/84）再建；否则新旧绑定互相踩。
9. **探测与替换**：结果码语义按设备校准；探测不明确（如 GET 返回 9）或实体与期望不一致时删除重建，而不是无限重试。
10. **失败分级**：GAL / bridge service profile / Alloc-ID / EVTO 增强项失败只记录；GEM、ANI 端口、至少一个端点端口失败才算阻断。

以上都落实后，才算"通用 profile 能接新型号"；单台设备通过不等于所有型号通过，验收仍按[分层矩阵](field-validation.md)执行。

当前通用代码要求完整 MIB，优先复用匹配分配，否则选择未分配 T-CONT；发现真实 UNI/VEIP 并保留旧端点顺序。当前共享 GEM 创建逻辑不等于对所有厂商的队列策略都成立。勿把案例中的队列候选写进通用模板而没有验证。

## 构建与验证

优先按当前 `README.md` 执行构建，而不是复用历史命令。当前 `all-devices` 仅组合设备 feature；`acs`、`mqtt` 需显式选择。默认 MQTT 为 fttrd 进程内 rumqttd；现场是否迁移须另查，不能照旧假设独立 Mosquitto。

当前 Windows 示例在 `h3c-fttr-tools/src`：

```powershell
$previous = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo zigbuild --release --locked --target aarch64-unknown-linux-musl --no-default-features --features all-devices,acs,mqtt --bin fttrd --bin fttrctl
} finally {
    $env:RUSTFLAGS = $previous
}
```

Linux/WSL 执行相关测试并记录既有失败；不要把 Windows 缺少 Unix API 的失败归因于新适配。交叉构建通过只证明可生成产物。只有在审查测试副作用并确认隔离后，才在设备运行选定测试；不在生产路由器盲跑全套测试。
