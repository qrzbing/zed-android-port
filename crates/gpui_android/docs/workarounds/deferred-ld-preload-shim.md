# LD_PRELOAD libzed-compat.so path-redirect shim (deferred)

**Status:** Deferred

Generic path-redirect shim for /etc/resolv.conf and similar Android-vs-Linux mismatches. Replaces the proot wrap for dynamic binaries (faster, no ptrace overhead). Static binaries (Bun-compiled) still need proot — LD_PRELOAD can't intercept statically-linked libc syscalls. Build infra (build.rs that compiles a .c via NDK clang into APK assets, extracted at boot) is the main cost; ~3-4 hr investment.

**Detailed writeup: TODO.** Stub created so the index links resolve.
