# 开发约定

阅读 `EVIDENCE/ENV.md` 和 `README.md` 获取本地编译环境、feed 集成和设备能力边界。

- 驱动源码位于 `h3c-fmcs/`，用户态源码位于 `h3c-fttr-tools/src/`。
- 设备适配集中在 `src/devices/<device>/`，按 Cargo feature 编译期裁剪。
- 修改持久化服务时核查 `files/fttr.init`、`files/fttr.config` 和 OpenWrt Makefile。
- OMCI 回读、真实 GEM 数据和真实 CWMP RPC 分别验证；不得互相替代。
- 未经用户明确要求，不提交或推送，不覆盖现场账号、密码及已有配置。
- 在代码和测试中，对 SN 等信息进行脱敏处理。
- rust代码变更后可执行cargo fmt --all
