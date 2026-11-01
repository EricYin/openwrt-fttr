---
name: fttr-device-onboarding
description: Use when onboarding a new FTTR ONU or sub-gateway in openwrt-fttr, investigating OMCI/GEM/CWMP compatibility, triaging OMCI result codes, or choosing a generic versus device-specific adapter. 用于新光猫、子猫的设备对接、证据采集、结果码定位与分层验收。
---

# FTTR 新设备对接

目标：建立可复现、可接续的设备对接过程，产出能力档案、适配决策和分层验证记录。设备未成功接入也必须留下可用结果。

## 开始前

1. 阅读仓库根目录 `AGENTS.md`、`README.md`、`EVIDENCE/ENV.md`，核实本轮源码、构建 feature、服务归属和现场版本。修改设备代码前再读 `h3c-fttr-tools/src/src/devices/AGENTS.md`，按注册层、厂商层、型号层组织代码。环境文件含私有信息，只在授权环境使用，禁止复制进共享记录。
2. 检查已有工作区变更并保留；确认本轮目标是发现、实现还是现场部署，以及用户允许的操作。独占窗口不自动等于允许恢复出厂、覆盖配置或固件升级。
3. 从[记录模板](templates/onboarding-record.md)建立本轮记录。以完整设备身份定位，但共享记录只使用 `<ONU_A_SN>` 等稳定别名。
4. 开始发现/适配前读[能力发现与适配决策](references/discovery-and-adapter.md)；开始现场实验前读[现场验证](references/field-validation.md)；出现重试循环或"配置成功但不通"时读[OMCI 结果码与数据面定位](references/omci-failure-triage.md)。
5. 需要只改 userspace 就上机验证（不整包刷机）时读[Linux 本地交叉构建与部署](references/local-build-and-deploy-for-linux.md)，Windows 构建另见仓库 `README.md`；需要案例时读[STAR 脱敏案例](references/star-sg631z-c-case.md)与[HWTC/SPGMAC 脱敏案例](references/hwtc-hg8010c-spgmac-case.md)。

## 执行阶段

| 阶段 | 必做动作 | 阶段产物 |
| --- | --- | --- |
| 基线 | 身份、注册、进程版本、其他 ONU、现有配置、当前症状 | 带采集时间的基线；明确未知项 |
| 发现 | 完整 MIB、关键属性、端点与资源关系、读写能力 | 能力清单与阻断位置 |
| 定位 | 失败的操作、类、实例、掩码、结果码；请求/响应成对报文；宿主侧门控与接线 | 阻断层级判定（OMCI 语义 / 宿主门控 / 数据面 / 接线） |
| 决策 | 判断标准实体布局差异、标准流程差异或厂商私有行为 | 通用改进或独立适配的证据理由 |
| 实现 | 最小改动、资源归属、重试幂等性、已有设备兼容性 | 针对性回归验证与构建产物 |
| 实验 | 一次一个变量、身份复核、前后状态、恢复路径 | 假设支持/排除/不确定及原始证据索引 |
| 验收 | 注册、OMCI、真实 GEM、管理网络、真实 CWMP 分别验证 | 分层结果，不能用一层替代另一层 |
| 交接 | 清理临时资源，保留现场配置，标明下一步 | 设备档案、实验记录、验收矩阵 |

## 判定规则

- “完整 MIB 没有该实体”“属性未解码”“请求不支持”“响应失败”是四种结论。
- **OMCI 结果码要按设备校准**：同一结果码在不同厂商实现里含义不同（尤其 4/5/7/9）。不要把"掩码不支持"读成"实体不存在"，也不要把"已存在"当成"设备忙"无限重试。详见[定位参考](references/omci-failure-triage.md)。
- **PLOAM 串号里的厂商码与 OMCI ME 256 的 ONU-G VendorId 是两个值**（案例中 `SPTC` 与 `SPGA`）。不要用其中一个校验另一个，也不要据此判定"不是同一台设备"。
- ONU ID 会变化；每次有副作用的操作前重新核对目标身份和注册代次。ONU 自身的 MIB 是持久的，**换槽位后旧槽位的 GEM/ANI 绑定仍在**，必须先退役再建，且实体编号要幂等。
- **优先复用 ONU 已有的桥端口与 EVTO**（按 TpPointer / AssociatedMePointer 匹配）；给同一个 UNI 再建一套会撞 PortNum，并覆盖厂商自己的管理 VLAN 计划。
- 宿主侧也有门控：`fttrN` 没有 carrier 时内核直接丢弃下行帧，而健康监控又会因活体判定失败拆掉 carrier。先证明门控条件，再怀疑 OMCI。
- 现场接线可能把 ONT 的 LAN 环路接回主猫 LAN，形成广播风暴并挤掉 OMCC 授权窗口，表现像"数据通了又断"。先查环路与风暴，再看协议。
- O5、配置回读、历史计数、carrier 或 UI 的 verified 标志均不能单独证明当前双向转发。
- 不把业务 VLAN、管理 VLAN、内部 GEM 封装标签和 ONU ID 当成同一个概念。
- 原生 CWMP ACS URL、OMCI ACS 关联、Connection Request URL 分别记录；当前配置不等于出厂默认值。
- ME 134 的地址字段不能独立证明厂商管理 WAN 的状态；先确认其关联，以设备 WAN、租约和实际请求响应交叉验证，不向任意 IP Host 实例盲写开关。
- `waiting_inform` 要区分传输、HTTP/SOAP 会话与身份关联：真实 Inform 可能已到 ACS，但光侧与 CWMP 标识尚未匹配。只对有证据的厂商/型号建立规则，并拒绝歧义匹配。
- 抓包点的静默不是设备未发包的充分证据；核查管理桥路径，按当前目标过滤并记录窗口。配置的上报周期也不等于已测量的实际节奏。
- 不根据一次报文、安静窗口或代码推测宣布根因；受控复现不足时明确标为假设。
- 对现场配置决策确有阻塞时说明最小下一步及影响；对能通过只读证据回答的问题继续调查。

## 脱敏与输出

共享 skill、案例、报告、日志摘录必须脱敏：完整/部分 SN、MAC、IP、主机名、用户名、本机绝对路径、端点 ID、客户配置标识使用稳定占位符。密码、令牌、Cookie、认证头、LOID 等不记录其值，使用 `<REDACTED>`。
URL 使用 `<ACS_URL>`；删除路径/查询中的令牌，不只替换主机。原始包、DHCP 选项和 CWMP 参数也可能含身份与凭据，先过滤再导出。若需保留原始证据，留在授权的私有位置，共享材料只记脱敏索引。
型号、公开固件版本、标准 ME 类号/实例、属性掩码、协议 VLAN 示例可以保留，必须标注来源和历史性，不能直接作为新设备默认值。

最终报告必须包括：本轮推进了什么、分层证据、未解决项、适配决策、下一步区分实验、临时资源与现场状态。不要将“调查完成”写成“设备适配成功”。

## 维护本 skill

增加新案例前按当前源码重核路径与 CLI；保留历史案例的时间边界，不用新源码推断旧现场已部署。任何参考文件失效时先修正引用。更新后检查相对链接、frontmatter 和隐私残留。项目级 skill 统一维护在 `.agents/skills/`，并通过符号链接兼顾 `.opencode/skills/` 与 `.claude/skills/`。
新增证据方法后，同步检查 `templates/onboarding-record.md` 是否有对应字段，避免"结论有了、记录没处写"。
