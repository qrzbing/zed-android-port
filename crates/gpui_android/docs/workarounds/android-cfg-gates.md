# audio + livekit_client + call cfg-gates

**Status:** Active

These crates pull libwebrtc and the upstream livekit crate, neither of which compile on aarch64-linux-android. The crates already have freebsd/windows-gnu mock fallbacks; we add target_os = 'android' to those cfg gates so the mocks compile cleanly. No behavioural change for other platforms.

**Detailed writeup: TODO.** Stub created so the index links resolve.
