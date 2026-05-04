# targetSdk=28 for execve from /data/data

**Status:** Active

At targetSdk≥29 SELinux blocks execve on app_data_file. We pin targetSdk=28 to keep `untrusted_app_27` domain semantics where execute_no_trans is allowed. Future-proof escape: Termux's system_linker_exec pattern (run via /system/bin/linker64 <bin>) when we eventually have to bump.

**Detailed writeup: TODO.** Stub created so the index links resolve.
