# `ZedApplication` for AppCompatActivity native lib load

**Status:** Active
**Phase / Commit:** L7
**Files:**
- `crates/gpui_android/examples/zed_android/android/app/src/main/kotlin/dev/zed/zed_android/ZedApplication.kt`
- `crates/gpui_android/examples/zed_android/android/app/src/main/AndroidManifest.xml`

## Problem

`ExtraWindowActivity extends AppCompatActivity` — a thin SurfaceView host
for our extra windows. On its first JNI call (`nativeOnExtraActivityCreated`),
we hit `UnsatisfiedLinkError` because `libzed_android.so` was never loaded.

MainActivity didn't have this problem: it `extends GameActivity`, which
loads the native library automatically by reading
`<meta-data android:name="android.app.lib_name">` from the manifest at
Activity creation time. AppCompatActivity has no such hook.

## Constraint

`System.loadLibrary` only needs to run *once* per process, but it must run
*before* any JNI call from anywhere in that process. Possible places to put
it:

1. **Per-Activity `companion object init` block** — fragile across
   recreation. Init-blocks fire on class load; if Android recreates an
   Activity instance whose class was already loaded in this process, the
   block doesn't re-run, which is fine (lib is already loaded), but if
   the loading races with other class-loading work it gets murky.
2. **`Application.onCreate`** — runs exactly once, at the very start of
   the process, before any Activity. Deterministic, centralized.

Per advisor feedback: option 2 is the textbook pattern. Also avoids
duplicating `System.loadLibrary("zed_android")` across MainActivity (which
already gets it via meta-data) and ExtraWindowActivity (which would need an
explicit init block).

## Solution

Add a thin `Application` subclass:

```kotlin
class ZedApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        System.loadLibrary("zed_android")
    }
}
```

Register in manifest:

```xml
<application android:name=".ZedApplication" ...>
```

Now both Activities are JNI-ready before they instantiate. The
`<meta-data android:name="android.app.lib_name">` on MainActivity stays
(GameActivity uses it for its own setup); the `loadLibrary` in
`ZedApplication.onCreate` is additive (idempotent — `System.loadLibrary`
of an already-loaded library is a no-op).

**Verified:** MainActivity has no `companion object init { System.loadLibrary }`
block. The lib gets loaded by GameActivity via the meta-data path. Don't
add a per-Activity loadLibrary — it would race with the Application path
on cold launch, no-op on warm.

## Why this works

`Application` is the first object Android instantiates in a process. Its
`onCreate` runs before any Activity, Service, or BroadcastReceiver. Loading
the native lib here makes JNI safe everywhere else.

## Failure mode if regressed

- `UnsatisfiedLinkError: No implementation found for boolean
  dev.zed.zed_android.NativeBridge.nativeIsExtraWindowKnown(long)` on first
  ExtraWindowActivity launch.
- App crash, no Settings window.

## See also

- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
