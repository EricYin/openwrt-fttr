include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-fttr
PKG_VERSION:=2026.11.07.080032
PKG_RELEASE:=1
PKG_LICENSE:=Apache-2.0

PKG_SOURCE_DATE:=2026-11-07
PKG_SOURCE_VERSION:=94c38c4b15e491335b72fca5c1877d556f9d6314
PKG_SOURCE:=$(PKG_NAME)-nightly.tar.gz
PKG_SOURCE_URL:=https://github.com/LazuliKao/luci-app-fttr/releases/download/nightly/
PKG_HASH:=541035d76e6d2c7f60defdb208df1bb472209f380039a733f122a35a114dc8d6

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
