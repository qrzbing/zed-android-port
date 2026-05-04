# Load bundled themes via LoadThemes::All

**Status:** Active
**Phase / Commit:** `bf0fd3f3c2` — Load bundled themes via LoadThemes::All

theme::init takes a LoadThemes enum. Default-on-platforms loads only common themes lazily; on Android we want every bundled theme available immediately so the picker has the full list. Pass LoadThemes::All instead of the platform default.

**Detailed writeup: TODO** — full text TBD next time the area changes.
