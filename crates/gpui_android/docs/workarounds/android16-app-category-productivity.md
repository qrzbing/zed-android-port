# `appCategory="productivity"` to defang the games carve-out

**Status:** Active
**Phase / Commit:** L7
**Files:** `crates/gpui_android/examples/zed_android/android/app/src/main/AndroidManifest.xml`

## Problem

Android 16's desktop windowing has a **games carve-out** — apps that look
like games are excluded from freeform multi-window behaviors and forced
into fullscreen / aspect-locked modes. Games get sized differently, are
exempt from forced resize, and behave more like phone apps.

We `extends GameActivity` for the primary surface (it's the easiest path
to a Vulkan-rendering NDK loop on Android). The OS may apply the games
heuristic to us, classifying Zed as a game and stripping desktop windowing
features. End-state: Settings doesn't open as a freeform window — opens
fullscreen instead.

## Constraint

The games heuristic at higher `targetSdk` values picks up on cues like
`extends GameActivity`, the `<meta-data android:name="android.app.lib_name">`
hint, and the `compileSdk` level. We're at `targetSdk=28` (pinned for
Termux execve, see [`targetSdk-28-execve`](targetsdk-28-execve.md)) so
behavior is murky — older targetSdk may be exempt from some new heuristics
but we shouldn't rely on it.

There's no API to "opt out of games classification." But there is an
explicit category override.

## Solution

Declare `android:appCategory="productivity"` on the `<application>` tag:

```xml
<application
    android:name=".ZedApplication"
    android:appCategory="productivity"
    android:resizeableActivity="true"
    ...>
```

Cheap insurance. Tells the OS "I'm a productivity app, not a game" —
applies to the games carve-out heuristic regardless of how GameActivity
inheritance looks from the outside.

## Why this works

`appCategory` is a documented manifest attribute (since API 26) that the
window manager and other system services consult when deciding which
behavior buckets to apply. Setting it explicitly to `productivity`
overrides any auto-detection that might bucket us as a game.

Other valid values: `accessibility`, `audio`, `image`, `maps`, `news`,
`social`, `video`, plus `undefined` (default). `productivity` is the right
fit for an editor.

## Failure mode if regressed

- In DeX/desktop windowing on Android 16+: Zed launches fullscreen
  instead of in a freeform window. ExtraWindowActivity may also be
  forced fullscreen.
- Subtle regression — won't crash, just looks wrong.

## See also

- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
- [`targetSdk=28` for execve](targetsdk-28-execve.md) — why we can't just bump targetSdk to dodge old heuristics
