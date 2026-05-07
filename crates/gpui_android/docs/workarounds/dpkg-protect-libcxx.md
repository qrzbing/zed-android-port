# dpkg path-exclude on libc++_shared.so

**Status:** Active

`$PREFIX/etc/dpkg/dpkg.cfg.d/zed-protect-libs` adds `path-exclude=$PREFIX/lib/libc++_shared.so` so dpkg refuses to extract that file from any package.

**Why:** The Termux apt repository ships a `libc++` package whose `libc++_shared.so` differs from the one we ship in our bootstrap zip. Our shipped binaries (apt, patchelf, claude when installed via npm) link against our specific build; replacing it with the upstream version causes:

- apt segfaults on its next invocation (`libapt-pkg.so` can't resolve a libstdc++ symbol against the new file)
- patchelf segfaults at `0x1423c0` when asked to set RPATH on the replaced file
- claude (Bun-compiled musl-static) segfaults the same way at the first PLT call

The trigger is normally `apt --fix-broken install`, which runs automatically when a user installs e.g. `rust` and pulls `ndk-sysroot` as a transitive dep. The `ndk-sysroot 29-2` upgrade chain forces a `libc++` upgrade, dpkg unpacks the libc++ deb, our `lib/libc++_shared.so` is overwritten, and the cascade begins. Without the path-exclude, recovering required uninstalling the app to wipe `$PREFIX` and re-extract from the APK.

**Defense in depth:** The apt patchelf hook (`98-zed-patchelf` Post-Invoke) also has a basename skip-list for `libc++_shared.so`, `ld-musl-aarch64.so.1`, and `libc.musl-aarch64.so.1`. patchelf grows ELF files by ~10% to add an RPATH section; doing that to the dynamic linker (ld-musl) is meaningless, and doing it to libc++_shared.so shifts section table offsets in ways that break the libstdc++ chain.

**Verifying:** `dpkg -S $PREFIX/lib/libc++_shared.so` should report `libc++: ...` (the package owns the path) but the file on disk should remain our shipped 1374336-byte version after any `apt install` that pulls libc++ as a dep. If the file ever balloons to ~1555753 bytes, the path-exclude isn't taking effect — likeliest cause is `etc/dpkg/dpkg.cfg.d/zed-protect-libs` was deleted from `$PREFIX` (re-installed every boot by `apply_runtime_patches`).
