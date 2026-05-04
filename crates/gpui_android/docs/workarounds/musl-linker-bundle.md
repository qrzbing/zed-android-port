# Musl-aarch64 linker bundled in APK

**Status:** Active

ld-musl-aarch64.so.1 (~723 KB) extracted from Alpine's musl-1.2.5-r23.apk and shipped as an APK asset. install_musl_linker copies it to $PREFIX/lib/ld-musl-aarch64.so.1 at boot plus a libc.musl-aarch64.so.1 symlink (in musl, the dynamic linker IS libc). Lets musl-dynamic binaries (Bun outputs, claude.exe) load their interpreter from a path that actually exists on Android.

**Detailed writeup: TODO.** Stub created so the index links resolve.
