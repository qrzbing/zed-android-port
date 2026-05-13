# Zdroid backlog

Non-blocking UX / feature gaps to revisit after the current refactor lands. Tasks in the live task list are atomic and time-bound; this file is for "noted but not yet scoped" concerns.

## "Open Project" dead on first boot, works after restart

**Repro:** fresh install (or after `pm clear com.zdroid`). Go through onboarding (Basics / AI / Editing / etc.), tap **Finish Setup**. Welcome page appears with the **Open Project** button. Tapping it does nothing. Close the app and reopen it; now the same button opens the SAF picker normally.

**Code path on tap** (from `crates/workspace/src/welcome.rs:125`):

```
SectionButton.on_click
  -> focus_handle.dispatch_action(&Open::DEFAULT)            welcome.rs:125
  -> Workspace Open action handler                            zed.rs:867
       -> workspace::prompt_for_open_path_and_open
            -> cx.prompt_for_paths(...)
                 -> AndroidPlatform::prompt_for_paths         platform.rs:780 (logs "invoked")
                      -> saf::pick_folder                     saf.rs:37 (logs "pick_folder requested")
                           -> MainActivity.launchOpenTree()   via JNI
                                -> startActivityForResult(OPEN_DOCUMENT_TREE)
```

**Likeliest causes** (in priority order, narrow with logcat capture during a failed first-tap):

1. **WelcomePage focus tree not attached to Workspace.** First boot goes through `show_onboarding_view` (`onboarding.rs:185`) -> `open_new` with an `Onboarding` pane item. After `Finish`, `go_to_welcome_page` (`onboarding.rs:450`) does `pane.add_item(WelcomePage, activate=true, focus=true)` then `pane.remove_item(onboarding_id, ...)`. If the focus update races the remove, the WelcomePage's `focus_handle` may not be inside the Workspace's focus tree at click time, so `dispatch_action` bubbles into a tree that has no `Open` handler.
2. **Activity-result wiring incomplete on first onCreate.** `MainActivity` registers its `ActivityResultLauncher` somewhere; first onCreate may not yet have wired the result-callback when the SAF intent fires, so `launchOpenTree` launches but the picked URI never reaches `Java_com_zdroid_MainActivity_onPickerResult` (`saf.rs:126`).
3. **`with_active_or_new_workspace` spawning a duplicate.** Same class of bug as documented in `crates/gpui_android/docs/workarounds/with-active-or-new-workspace-android-fallback.md`; the action handler isn't routed via that shim, but related multi-window state may leave the action lost in a freshly-spawned-and-discarded MultiWorkspace.

**Diagnostic next step:** capture `adb logcat --pid=$(adb shell pidof com.zdroid)` from app launch through the failed Open Project tap. Look for `AndroidPlatform::prompt_for_paths invoked` and `saf: pick_folder requested`. If neither appears, the action never reached the platform (focus / handler issue, cause 1 or 3). If both appear, the platform fired correctly and the SAF Activity-result is the problem (cause 2).



## Reported via the @-mention bug-can't-be-filed loop (2026-05-13)

External user tried to file these and was blocked by upstream Zed's `blank_issues_enabled: false` + Discord-redirect contact_links (fixed in `244f29a455`). Logging here so they don't get lost while the user re-files via the new templates.

- **Mouse scroll direction reversed**: Samsung One UI + Bluetooth mouse. Scroll works correctly everywhere else on the device but is inverted in Zdroid. Likely a sign flip in `gpui_android`'s `MotionEvent` → scroll-delta translation. Check the wheel-axis path specifically (touch-pad two-finger scroll may differ); One UI may report `AXIS_VSCROLL` with a sign that mainline Zed doesn't compensate for. **Still open.**
- ~~**Vim mode broken**~~ — **fixed in 29b29ddd63** (key events now route from `ExtraWindowActivity` to gpui via `nativeOnExtraKeyEvent` JNI bridge; the root cause was that any extra window's `dispatchKeyEvent` was never forwarded so `PlatformInput::KeyDown` never fired). Vim and any other editor in a non-Main window now accepts typing.
- ~~**Settings searchbar typing broken**~~ — **fixed in 29b29ddd63** (same key-routing fix, plus a tap-to-focus wrapper on the search bar with `stop_propagation` so taps don't bubble to `SettingsWindow`'s focus tracker which was immediately blurring the bar).

## Runtime picker: adapter install-state UX (deferred)

The onboarding runtime-picker section (and the Settings page entry) currently presents all three adapters — Kali chroot, Bootstrap, External Termux — as selectable equals. On a fresh install the user almost certainly has NONE of them installed:

- **Chroot** requires Magisk + the `zdroid-spawnd` Magisk module (separate flashable zip from GitHub releases) + a Kali NetHunter chroot rootfs at `/data/local/nhsystem/kali-arm64`. None of those are bundled in our APK.
- **Bootstrap** requires the bootstrap zip (~240 MB) extracted into `$PREFIX`. Auto-extracted from the APK asset today, but per task #17 we're moving to GitHub-release download. If the asset isn't present, "Bootstrap" doesn't work either.
- **External Termux** requires the Termux app installed on the device AND the user granting `com.termux.permission.RUN_COMMAND`. Plus our JNI Intent bridge (task #36) needs to land.

The picker doesn't surface any of this. A user tapping "Bootstrap" with no bootstrap installed sees: their selection saved, restart Zdroid, nothing works, no actionable feedback. Same for chroot without the Magisk module. Worst-case UX.

**Bridge needed:**

- Show an install-state badge per adapter (Installed / Missing / Partial).
- For Missing: link out to the install instructions (GitHub release page, F-Droid Magisk module link, Termux Play Store/F-Droid).
- For Partial (e.g., chroot module installed but rootfs path is empty): explain what's missing AND what to do.
- Consider whether to install silently (downloads bootstrap zip on selection) or surface as an opt-in download with progress.
- "Auto-install" vs "show options" — likely the right answer is BOTH, with auto-install gated behind explicit user consent because: bootstrap is 240 MB, chroot requires Magisk + root, external Termux needs a separate app install. Silent install is hostile to discoverability and bandwidth control.

The chroot adapter already has health-check hints in `RuntimeProvider::health_check()` — `HealthStatus::NotInstalled { hint }` with links to the spawnd release page. The picker UI just doesn't surface these yet. Wiring through to the inline section would give us the install-state-aware UX without inventing new infrastructure.

**Related tasks already in the live list:**

- #33 — wire settings UI for adapter selection
- #34 — first-launch onboarding modal (now landed as the inline basics_page section, but install-state UX is still TBD)
- #36 — JNI Intent bridge for external Termux adapter
- #37 — bootstrap install/uninstall (GitHub releases download)

Resolve this AFTER the Termux-divestment refactor (tasks #74-80) lands. Once each adapter is a first-class peer with a proper install flow, the picker can surface that flow.

## Settings search bar input pipeline (deferred)

Tapping the settings search bar on Android doesn't bring up the soft keyboard and the Editor never receives input events. Probe instrumentation in `crates/settings_ui/src/settings_ui.rs` shows:

- Editor entity is created (we see `editor created` at boot)
- A `Blurred` event fires after the user taps, but no `Focused` event reaches the subscribe handler
- No `InputHandled` / `InputIgnored` / `Edited` events

So the Editor IS getting focused (Blurred can only fire after a Focused state), but the focus is being stolen immediately, AND the Android InputMethodManager (soft keyboard) is not being prompted. Two suspected layers:

1. **GPUI / gpui_android focus handling**: the on_click handler I added (focus the editor handle on bar click) appears to set focus but lose it instantly. Could be the welcome page or some other view stealing it back via focus rules.
2. **Android IME wiring**: even if focus stays on the Editor, the soft keyboard doesn't open. Zed-the-app needs to call `InputMethodManager.showSoftInput()` (Java/JNI) when an Editor gains focus on Android. This wiring is in gpui_android's platform layer and may be incomplete.

Diagnosing requires instrumenting gpui_android's input layer too, not just the editor subscription. Probably want JNI logs on `onWindowFocusChanged`, `dispatchKeyEvent`, and `InputMethodManager` calls. Out of scope for the Termux-divestment refactor; pick up after Phase 8.

Visible workaround for users today: there isn't one — search filter is effectively dead on Android. Settings remain navigable via the sidebar tree.

## Other deferred concerns

(add new sections here as they come up; keep each one self-contained with what / why / what's-needed-to-resolve)
