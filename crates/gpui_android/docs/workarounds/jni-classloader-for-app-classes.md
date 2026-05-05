# JNI ClassLoader for app classes

**Status:** Active
**Phase / Commit:** L7
**Files:** `crates/gpui_android/src/multi_window.rs` (`launch_extra_activity_inner`)

## Problem

Building an `Intent(ctx, ExtraWindowActivity.class)` from JNI on Android
needs a `Class<?>` reference for our own app's `ExtraWindowActivity`. The
obvious idiom is `Class.forName("dev.zed.zed_android.ExtraWindowActivity")`,
which works fine in plain JVM. On Android it throws:

```
java.lang.ClassNotFoundException: dev.zed.zed_android.ExtraWindowActivity
```

…and worse, the JNI exception abort tripped a process abort
(`JNI DETECTED ERROR IN APPLICATION: JNI GetObjectClass called with pending
exception`) before we could even log a clean error.

## Constraint

Android partitions classloaders per app. The system classloader (which
`Class.forName(name)` consults by default) only knows framework classes —
`android.*`, `java.*`, etc. App classes (anything from `/data/app/<pkg>/base.apk`)
live in a *separate* classloader specific to the package. The system loader
cannot see them.

The same constraint applies to `JNIEnv::find_class` from a non-UI thread:
the JNI thread doesn't inherit the app classloader unless attached via the
right path. Our `attach_current_thread` from the game thread doesn't.

## Solution

Resolve the class through the *Activity's* classloader, which knows about
the app's APK contents. MainActivity is reachable via
`AndroidApp::activity_as_ptr()`, so we grab its class and pull the loader
off it:

```rust
let main_class = env.get_object_class(&main_activity)?;
let class_loader = env
    .call_method(&main_class, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
    .l()?;
let class_name = env.new_string("dev.zed.zed_android.ExtraWindowActivity")?;
let extra_class = env
    .call_method(
        &class_loader,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[JValue::Object(&class_name)],
    )?
    .l()?;
```

`loadClass` on the Activity's loader does see app classes. The result is
a `Class<?>` we can pass to `new Intent(ctx, cls)`.

## Why this works

App classloaders are real `ClassLoader` instances reachable through any
app-supplied object's `Class.getClassLoader()`. Whatever loaded MainActivity
loaded ExtraWindowActivity too (same APK, same loader). We borrow it.

This is the same pattern Android-NDK samples use when bridging from native
threads to app classes.

## Failure mode if regressed

- `ClassNotFoundException` at startActivity time → process abort if
  exception isn't cleared (see
  [JNI exception clear after error](jni-exception-clear-after-error.md)).
- Symptom in user-facing terms: tap "Open Settings" → app crashes silently.

## See also

- [JNI exception clear after error](jni-exception-clear-after-error.md) — must clear pending exception even on success path
- [Multi-Activity OS-chromed extra windows](multi-activity-os-chrome.md)
