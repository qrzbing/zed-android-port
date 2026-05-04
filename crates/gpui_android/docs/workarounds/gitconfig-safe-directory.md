# .gitconfig safe.directory = *

**Status:** Active

Files under /storage/emulated/0 are owned by media_rw (UID 1023); we run as the app's per-app UID. libgit2's dubious-ownership check fires on every repo open. Pre-create ~/.gitconfig with [safe] directory = * at boot if not already present. Idempotent — never clobber a user's existing config.

**Detailed writeup: TODO.** Stub created so the index links resolve.
