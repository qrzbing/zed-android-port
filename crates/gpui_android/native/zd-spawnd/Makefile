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

# Convenience: stage the binary + Magisk module skeleton into a
# build/ directory ready to zip. (Magisk module structure is created
# in a separate task; this is a placeholder.)
install-magisk-module: $(OUT)
	@echo "Magisk module packaging is in a separate task — see"
	@echo "crates/gpui_android/native/zd-spawnd/magisk-module/ once added."
