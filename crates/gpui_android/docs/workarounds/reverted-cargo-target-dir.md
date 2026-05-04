# CARGO_TARGET_DIR redirect (reverted)

**Status:** Reverted

Briefly set CARGO_TARGET_DIR=$HOME/.cargo-target so cargo's target/ landed on app-private storage. Worked for cargo, didn't generalize to go/make/gcc/gradle/etc. Replaced by the Termux storage workflow (projects live under ~/projects/, all build tools work natively).

**Detailed writeup: TODO.** Stub created so the index links resolve.
