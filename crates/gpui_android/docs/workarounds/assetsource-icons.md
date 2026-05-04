# Thread AssetSource through gpui_android::run

**Status:** Active
**Phase / Commit:** `a99be4dc57` — Thread AssetSource through gpui_android::run
**Files:** `crates/gpui_android/src/lib.rs`, `crates/gpui_android/examples/zed_android/src/lib.rs`

## Problem

Every icon in the editor rendered as a blank rectangle: back/forward nav
arrows, file-tree folder/file glyphs, status-bar buttons, panel toggles,
etc.

## Constraint

GPUI's SVG renderer needs an `AssetSource` to resolve `IconName::*` lookups.
Without one, it falls back to a no-op `AssetSource` that returns nothing
for every key — every icon then renders as the empty fallback (a blank
rect). The asset source has to be threaded through `Application::with_assets`
during gpui setup; there's no global default we could rely on.

## Solution

`gpui_android::run` takes an `Arc<dyn AssetSource>` parameter and calls
`Application::with_assets(asset_source)` during the gpui boot chain. The
example crate wires up `RustEmbed`-backed assets (the same pattern
production zed uses):

```rust
gpui_android::run(asset_source, /* ... */, |cx| { /* user boot */ });
```

## Why this works

- gpui's SVG renderer queries the registered AssetSource at every icon
  paint. With the right source, every key resolves to the bundled SVG
  bytes.
- `RustEmbed` bakes the icon assets into the .so at build time, so no
  runtime filesystem dependency.

## Failure mode if regressed

- Pass `None` or a no-op AssetSource → every icon goes blank. Easy to
  spot visually but easy to break by refactoring the run signature.

## See also

- [pointer-icon-cursor-mapping.md](pointer-icon-cursor-mapping.md) — same
  pattern of "what was a no-op stub now actually does something"
