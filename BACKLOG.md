# Zdroid backlog

Non-blocking UX / feature gaps to revisit after the current refactor lands. Tasks in the live task list are atomic and time-bound; this file is for "noted but not yet scoped" concerns.

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
