# Construct buffer inside open_window to avoid borrow re-entry

**Status:** Active
**Phase / Commit:** `540dd170d1` — Construct buffer inside open_window

Sibling of refcell-drain-platform-bug. Constructing the buffer entity outside open_window's borrow scope and passing it in caused a re-entrant cx.update borrow when the buffer's own constructor wanted to update something. Constructing inside open_window keeps the borrow scope flat. Same class of bug as 72cbfd3973 (Skip runnable drain inside open_window).

**Detailed writeup: TODO** — full text TBD next time the area changes.
