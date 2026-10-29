# HWTC HG8010C 与 SPGMAC SFP ONT 对接案例（脱敏）

历史过程案例，不是设备默认配置，也不是当前现场状态。现场观察来自 2026-10-28；同日按当时工作区源码复核引用。
身份统一用 `<ONU_A_SN>`（华为单口桥接光猫）、`<ONU_B_SN>`（SFP ONT）、`<OLT_HOST>`、`<ONT_MGMT_IP>`；不提供真实身份映射。
型号 HG8010C、SPGMAC 与固件版本号属能力背景，不是凭据。

本轮目标是把"能接 H3C 子猫 / 单一口味"的实现做成**换型号插上就能用**，因此六个阻断都按"标准差异 vs 厂商私有"分类处理，尽量落在通用 profile 而不是新增设备适配。

## 1. 写死拓扑的厂商 profile 不通用

现象：新接入的 HG8010C 已 O5，但业务反复 `OMCI result=5`，掩码全零。

证据：当时选择逻辑把厂商码 `HWTC` 一律交给厂商 profile，该 profile 在写桥之前先探测固定实例 `262:0x8000`、`277:0x8000`、`11:0x101`、`11:0x102`、`329:0x601`。完整 MIB 只有 1 个 Ethernet UNI、无 VEIP、无桥、8 个空闲 T-CONT，第二个 UNI 探测即失败。

处理：选择逻辑只对**已验证型号**（该案例中是 K155-11/V152）保留厂商 profile，其余型号落到通用 profile；不再用"厂商码相同"推断"拓扑相同"。

验证：同一台设备改走通用 profile 后一次性完成 T-CONT/GEM/ANI/桥端口/EVTO 回读。

## 2. ME 268 上行 TM 指针必须指向 T-CONT

现象：SFP ONT 在创建 GEM CTP 时被拒，`result=3`，执行掩码 `0x1000`（属性 4）。

证据：`molt_omci` 抓到 `CREATE 268:0x100` 后紧跟 `result 03 10 00`；登录 ONT 后其 OMCI 守护进程日志给出同一结论：`SBC validation error (mask 0x1000)`，并且实现里明确"上行指针冗余指向 T-CONT 时跳过校验"。

处理：通用 profile 的 GEM CTP 一律把属性 4 写成该 GEM 的 T-CONT 指针；对历史版本留下的 TM=0 实体做一次修复（读到就改，读不到就跳过）。

验证（受控、可恢复）：先把 ONT 自身 MIB 模板预置一条带正确 TM 指针的 268，重启其 OMCI 守护进程，主猫随即走"已存在"分支并完成整条 profile（266/47/84/171 全部被接受）——这说明除该属性外没有别的报文问题。测后立即还原模板，并记录"运行态 MIB 与文件模板是两份状态"。

## 3. 健康监控用 PLOAM 厂商码比对 OMCI 厂商码

现象：carrier 拉起 15 秒后必被拆，30 秒后整轮重配，固定 45 秒节律。

证据：同窗口内 `molt_omci` 上 12 秒 5 个 GET(256) / 5 个应答，OMCI 本身完好；但串号厂商码是 `SPTC`、ME 256 的 ONU-G VendorId 是 `SPGA`，活体判定要求两者相等，于是永不记账。

处理：活体判定改为"PLOAM 厂商码 **或** 本会话首次校验通过的应答里的 VendorId"；信封 ONU-ID 与事务号已足以绑定会话。

验证：改后连续观察数分钟，不再出现 `stale OMCI evidence` / `watch lost ONU`。

## 4. carrier 门控死锁（宿主侧，不是 OMCI）

现象：profile 回读完全正常，但业务永远不通；从主猫侧发包完全没有反应。

证据：往目标 `fttrN` 发两帧，`tx_dropped` +2，而 `fttr0` 上没有对应 VLAN 帧——carrier=0 时内核直接丢弃，驱动的发送函数根本没执行；而 carrier 又要等"收到 ONU 上行帧"才拉起，形成互相等待。

处理：carrier 由"OMCI 活体"驱动（`GEM_RX=false` 也拉起），业务状态另等 GEM 证据；日志里把 `GEM_RX` 明示出来避免混淆。

验证：改后立刻 `VERIFIED_LINK_ENABLED OMCI_GET=true GEM_RX=false`，`fttrN` 进入 forwarding；随后 ONT 侧设备的 DHCP/ARP 上来，`DATA_GEM_RX_CONFIRMED` 出现。

## 5. 现场接线环路伪装成"时通时断"

现象：数据看起来通了，几秒后又断；目标 `fttrN` 的 tx 包速上万。

证据：内核报 `received packet on lan1 with own address as source address (addr:<fttrN_MAC>)`；`fttrN` tx 计数短时间数百万包。原因是把 ONT 的 LAN 又用铜线接回了主猫 LAN，与"ONT LAN → PON → fttrN → br-lan"形成环路，且 br-lan 未开 STP。

处理：断开冗余铜线，ONT 的 LAN 只接终端；临时止血可先把该 `fttrN` 从网桥摘下，并记录恢复动作。

验证：风暴停止后 OMCI 采样恢复，carrier 不再周期性被拆。

## 6. ONU ID 互换后的陈旧 ANI 绑定

现象：重启主猫后两颗 ONT 的 ONU ID 互换，原先正常的 SFP ONT 变成 `existing entity differs class=47 instance=0x101` 无限重试。

证据：ONT 的 MIB 是持久化的，留着"上次作为另一个槽位"时创建的 `47:0x100 + 268:0x100`；新槽位要建 `0x101`，两者冲突；而且 ANI 端口的 PortNum 每次计算都不同，回读永远不一致。

处理：通用 profile 先退役指向其它槽位 GEM 的 ANI 绑定（`GENERIC_STALE_ANI_RETIRED`），并复用本槽位 ANI 端口已有的 PortNum，使重配幂等。

验证：改后两颗 ONU 同时完成回读，不再互相踩。

## 7. 华为对已存在的 ME 268 也回 result=9

现象：修好 TM 指针后，HG8010C 又陷入"探测→创建→失败"循环，`result=7`。

证据：抓包显示 `GET 268:0x100 mask=0xe000` → `result=9`（实体其实已存在，是它拒绝报告该掩码），随后 `CREATE 268:0x100` → `result=7`（已存在）。

处理：把"结果码 9"从"不存在"里剔除；探测不明确或实体内容不一致时**删除重建**；CREATE 回 4/7 时删除后重试一次；CREATE 成功但读不回则接受并记录。

验证：改后 HG8010C 走上 `REPLACING_UNREPORTED_ENTITY` → 完成回读 → `phase=verified`。

## 8. Lantiq 系 ONT 拒绝用 OMCI 写 T-CONT Alloc-ID

现象：`assign_tcont` 的 SET 返回 `result=3`。

证据：同一 ONT 依赖 PLOAM 的 Assign_Alloc-ID 决定 T-CONT 分配，OMCI 侧只读。

处理：通用 profile 把 Alloc-ID 写入降级为"失败即跳过并记录"，GEM 仍按已发现的 T-CONT 创建。

验证：profile 继续走完；数据面是否可用另行按 GEM 证据判定，不因跳过而宣称成功。

## 结果

| 项目 | 本轮结论 |
| --- | --- |
| 型号无关选择 | 只有已验证型号保留厂商 profile，其余走通用路径（已落地） |
| 通用 profile 能否一次接入 virgin ONT | 能（HG8010C 从零建出 T-CONT/GEM/ANI/桥/EVTO 并回读） |
| 复用出厂桥端口与 EVTO | 已落地；降低撞号与覆盖厂商管理 VLAN 的风险 |
| ONU ID 变化后的自愈 | 已落地（退役陈旧 ANI + 幂等 PortNum） |
| 两台 ONU 同时在线 | 均 `phase=verified`、carrier 处于 forwarding，华为侧出现真实 DHCP 与业务计数 |
| 自动按 SN 固定槽位 | 未实现；当前靠"注册顺序 + 自愈"，重启仍可能闪断 |
| ONT 自身管理 VLAN 是否保留 | 复用出厂 EVTO 后有改善方向，但未系统性验证 |

## 可复用经验

1. 先分层（OMCI 语义 / 我方配置 / 宿主门控 / 数据面 / 接线），再改代码；本轮前四个问题里有三个不在 OMCI 报文本身。
2. 结果码必须按设备校准，尤其是 9 和 7；"判不准就删除重建"比"猜测复用"收敛更快。
3. 通用 profile 的正确性来自"从 MIB 读"，而不是"模板写得更全"：T-CONT、端点、桥端口、EVTO、PortNum 都要以现场为准。
4. 设备身份、槽位、实体编号三者要分开管理；任何有副作用的操作前重新核对。
5. 需要现场快速验证时，用[本地交叉构建与部署](local-build-and-deploy.md)做临时二进制，正式产物仍走 feed 构建。
6. 下一步最值得做的是**按 SN 固定槽位**（省掉 ONU ID 漂移带来的重配与闪断）。
