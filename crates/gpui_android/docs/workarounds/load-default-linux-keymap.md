# Load default Linux keymap on boot

**Status:** Active
**Phase / Commit:** `c1a470dc30` — Load default Linux keymap on boot

Android's KeyEvent codes look closer to Linux's than to macOS's. Load the default-linux keymap (Ctrl-prefixed bindings, etc.) instead of the macOS keymap which would misroute Cmd-prefixed bindings. Picked at boot via assets::load.

**Detailed writeup: TODO** — full text TBD next time the area changes.
