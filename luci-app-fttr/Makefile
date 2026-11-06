include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-fttr
PKG_VERSION:=nightly
PKG_RELEASE:=1
PKG_LICENSE:=Apache-2.0

PKG_SOURCE_DATE:=2026-11-06
PKG_SOURCE_VERSION:=main
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://github.com/LazuliKao/luci-app-fttr/releases/download/nightly/
PKG_HASH:=skip

# Support local source directory override for local development if FTTR_LUCI_SRC is provided
ifneq ($(FTTR_LUCI_SRC),)
  PKG_BUILD_DIR:=$(FTTR_LUCI_SRC)/package/luci-app-fttr
endif

LUCI_TITLE:=FTTR management interface
LUCI_DEPENDS:=+luci-base +rpcd +h3c-fttr-tools
LUCI_DESCRIPTION:=LuCI overview for the FTTR controller and downstream ONUs.
LUCI_MAINTAINER:=LazuliKao

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
