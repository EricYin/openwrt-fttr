include $(TOPDIR)/rules.mk

PKG_NAME:=h3c-fttr-tools
PKG_VERSION:=0.1.0
PKG_RELEASE:=1
PKG_URL:=https://github.com/LazuliKao/openwrt-fttr/tree/main/h3c-fttr-tools

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1
PKG_CONFIG_DEPENDS:=CONFIG_FTTR_DEVICE_HL202 CONFIG_FTTR_DEVICE_HWTC CONFIG_FTTR_ACS CONFIG_FTTR_MQTT CONFIG_FTTR_MIMALLOC

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/h3c-fttr-tools
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=PON
  TITLE:=H3C HM2004-DU FTTR Micro-OLT utilities and controller daemon
  DEPENDS:=$(RUST_ARCH_DEPENDS) +kmod-h3c-fmcs +uci +ubus +firewall4 +dnsmasq +FTTR_ACS:curl
endef

define Package/h3c-fttr-tools/description
 Pure-Rust userspace FPGA programmer, optical BOSA management, and FTTR controller daemon for H3C HM2004-DU.
 Manages downlinked stock HL202-DU sub-gateways over optical bridge and MQTT.
endef

define Package/h3c-fttr-tools/config
  config FTTR_DEVICE_HL202
    bool "Compile HL202-DU-C adapter (verified business and MQTT commands)"
    depends on PACKAGE_h3c-fttr-tools
    default y
  config FTTR_DEVICE_HWTC
    bool "Compile HWTC adapters (standard OMCI and model profiles)"
    depends on PACKAGE_h3c-fttr-tools
    default y
  config FTTR_ACS
    bool "Compile embedded minimal CWMP ACS (runtime UCI switch)"
    depends on PACKAGE_h3c-fttr-tools
    default y
  config FTTR_MQTT
    bool "Compile controller-owned MQTT broker and clients"
    depends on PACKAGE_h3c-fttr-tools
    default y
  config FTTR_MIMALLOC
    bool "Use mimalloc for fttrd Rust allocations (experimental)"
    depends on PACKAGE_h3c-fttr-tools
    default n
    help
      Select mimalloc 3.5.3 for the daemon only, without explicit THP advice.
      Compare target RSS, swap and service behavior before enabling in production.
endef

RUST_PKG_FEATURES:=$(if $(CONFIG_FTTR_DEVICE_HL202),device-hl202) $(if $(CONFIG_FTTR_DEVICE_HWTC),device-hwtc) $(if $(CONFIG_FTTR_ACS),acs) $(if $(CONFIG_FTTR_MQTT),mqtt) $(if $(CONFIG_FTTR_MIMALLOC),allocator-mimalloc)
CARGO_PKG_ARGS+= --no-default-features

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./src/Cargo.toml ./src/Cargo.lock $(PKG_BUILD_DIR)/
	$(CP) ./src/src $(PKG_BUILD_DIR)/
endef

define Package/h3c-fttr-tools/install
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttrctl $(1)/usr/sbin/
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttrd $(1)/usr/sbin/
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttr-omci-service $(1)/usr/sbin/
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/fttr-omci-probe $(1)/usr/sbin/
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/fttr.init $(1)/etc/init.d/fttr
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/fttr.config $(1)/etc/config/fttr
	$(INSTALL_DIR) $(1)/lib/upgrade/keep.d
	$(INSTALL_DATA) ./files/fttr.keep $(1)/lib/upgrade/keep.d/fttr
endef

define Package/h3c-fttr-tools/conffiles
/etc/config/fttr
endef

$(eval $(call RustBinPackage,h3c-fttr-tools))
$(eval $(call BuildPackage,h3c-fttr-tools))
