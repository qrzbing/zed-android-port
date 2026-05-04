# Welcome-page Workspace/External split (Android)

**Status:** Active

cfg-gated to target_os=android. crates/workspace/src/welcome.rs partitions recent_workspaces by whether the path starts with TERMUX__HOME/projects. Workspace section = local; External = anywhere else (most often /storage/emulated/0 paths). Same-name projects from different storage tiers stop being indistinguishable.

**Detailed writeup: TODO.** Stub created so the index links resolve.
