# Phone form-factor polish (deferred)

**Status:** Deferred — niche audience, low priority

The Android port is built primarily for tablets and desktop-mode
Android (DeX, Pixel desktop windowing, Android 16 Desktop Mode,
ChromeOS), where there's enough screen real estate to run a real
code editor with multi-pane layouts, two side-by-side worktrees,
the agent panel, etc. Phone-sized devices (5"–7", 1080×2340 class)
work — the build runs, the renderer paints, files open, builds
compile inside Termux — but the UX is rough enough that we
explicitly de-prioritize it:

- **Soft keyboard takes ~half the screen** — once the IME bridge
  lands (`deferred-soft-keyboard.md` / Phase 8.8), the editor pane
  shrinks dramatically when typing, leaving 4–5 visible code lines
  on a tall phone. This is a Linux-on-Android problem too, not just
  ours; production phones meant for code editing don't really
  exist as a category.
- **Multi-pane layouts wrap into character-per-line columns** at
  narrow widths (saw this in the freeform Settings test on Mi 10:
  the right pane in `settings_ui` rendered each label letter on its
  own line). Fixing it requires a responsive layout pass on the
  side panels — collapse to single-pane below some width
  threshold, hide secondary chrome, etc.
- **Forced freeform on phone** is also broken on
  Mi 10 / MIUI: it crashes on launch with a JNI scope abort deep in
  Android's `setTitle → ServiceManager.getService → ClipboardManager.<init>`
  Binder chain (MIUI vendor customization that fetches
  ClipboardManager on title changes). Fixing that requires a
  Java-side `runOnUiThread` wrapper on `set_title` (see
  `multi_window::set_extra_activity_title`), but it's only worth
  doing if we commit to phone freeform as a target.
- **MIUI / HyperOS aggressively kills backgrounded Zed**
  (see [miui-aggressive-task-killing.md](miui-aggressive-task-killing.md)).
  The user-facing recovery (`Battery saver → No restrictions`) is
  documented but not great UX for the unusual user who only has
  a phone.

## Audience reasoning

The realistic target for "Zed on Android" is someone who:

1. Has a tablet or convertible (Galaxy Tab S9 Ultra, Pixel
   Tablet, ChromeOS box, foldable in tablet mode) where the
   screen is wide enough for the production layout, OR
2. Plugs an Android device into an external monitor + bluetooth
   keyboard + mouse for a desktop-mode session (Samsung DeX,
   Pixel desktop windowing, motorola Ready For), OR
3. Pairs a hardware keyboard with their tablet for typing-heavy
   work.

Devs who do "serious dev work for other Zed-on-Android os" use
case all fall into these. The user with **only** a 6" phone, no
external screen, no keyboard, doing real coding — that's a niche
we can address later if requested. For now the form-factor
optimizations and bug-fixes for that path stay deferred.

## What gets revisited later

If real users hit any of these and report:

1. Audit `settings_ui` (and other multi-pane crates) for narrow-
   width responsive collapse (~500dp single-pane breakpoint).
2. Wrap `multi_window::set_extra_activity_title` (and any other
   UI-thread Activity method calls from Rust) in a
   `runOnUiThread` Java helper to fix MIUI freeform crash.
3. Detect MIUI / HyperOS at launch and surface a one-time banner
   pointing at the battery-saver setting, OR call
   `PowerManager.requestIgnoreBatteryOptimizations` ourselves
   (limited but better than nothing).
4. Revisit IME bridge prioritization (Phase 8.8) — currently
   sitting at the back of the queue because hardware keyboards
   cover all our test paths.

## See also

- [deferred-soft-keyboard.md](deferred-soft-keyboard.md) — the
  IME bridge work that would unblock typing on phone
- [miui-aggressive-task-killing.md](miui-aggressive-task-killing.md)
  — MIUI battery-saver kill behavior
- [activity-options-launch-bounds.md](activity-options-launch-bounds.md)
  — how we set freeform window bounds (works on Tab S9 / DeX, not
  Mi 10 forced-freeform yet)
