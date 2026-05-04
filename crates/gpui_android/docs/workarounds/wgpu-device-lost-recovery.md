# Handle wgpu device-lost on Android

**Status:** Active
**Phase / Commit:** `cd7ec7d745` — Handle wgpu device-lost on Android

Android's GPU driver can lose the wgpu device under memory pressure or app backgrounding. Without explicit handling the renderer dies silently and the next frame is a black box. Catch the device-lost callback, drop and recreate the renderer, restore the swapchain. Atlas textures survive the recreation because we hold them in the platform layer not the renderer.

**Detailed writeup: TODO** — full text TBD next time the area changes.
