# zd-spawnd cross-compile Makefile.
#
# Outputs an aarch64 ELF binary linked against bionic libc (the device's
# system libc). Suitable for Magisk module deployment.
#
# Usage:
#   make                              # build for host arm64 device
#   make NDK=/path/to/ndk             # override NDK location
#   make API=24                       # override min API level
#   make clean

NDK       ?= /opt/homebrew/share/android-commandlinetools/ndk/27.0.12077973
HOST      ?= darwin-x86_64
TARGET    ?= aarch64-linux-android
API       ?= 26

TOOLCHAIN := $(NDK)/toolchains/llvm/prebuilt/$(HOST)
CC        := $(TOOLCHAIN)/bin/$(TARGET)$(API)-clang

CFLAGS   ?= -O2 -Wall -Wextra -Wno-unused-parameter -fPIE -fstack-protector-strong
LDFLAGS  ?= -pie -Wl,--gc-sections

OUT      := zd-spawnd

.PHONY: all clean install-magisk-module

all: $(OUT)

$(OUT): zd-spawnd.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)
	@echo "built $@"
	@$(TOOLCHAIN)/bin/llvm-readelf -h $@ | grep -E "Machine|Class" || true

clean:
	rm -f $(OUT)

# Package the daemon + magisk-module/ into a flashable Magisk zip.
# Output: build/zdroid-spawnd-<version>.zip in this directory.
MODULE_DIR    := magisk-module
MODULE_VERSION ?= $(shell grep '^version=' $(MODULE_DIR)/module.prop | cut -d= -f2)
ZIP_NAME      := zdroid-spawnd-$(MODULE_VERSION).zip
BUILD_DIR     := build

magisk-module: $(OUT)
	@rm -rf $(BUILD_DIR)
	@mkdir -p $(BUILD_DIR)/staging
	@cp -r $(MODULE_DIR)/. $(BUILD_DIR)/staging/
	@cp $(OUT) $(BUILD_DIR)/staging/zd-spawnd
	@cd $(BUILD_DIR)/staging && zip -r ../$(ZIP_NAME) . >/dev/null
	@echo "built $(BUILD_DIR)/$(ZIP_NAME)"
	@unzip -l $(BUILD_DIR)/$(ZIP_NAME) | tail -n +4

clean-magisk-module:
	rm -rf $(BUILD_DIR)

.PHONY: magisk-module clean-magisk-module
