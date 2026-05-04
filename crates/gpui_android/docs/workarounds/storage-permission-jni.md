# Storage permission JNI shim

**Status:** Active

MainActivity.kt has requestStoragePermissions() that prompts for READ/WRITE_EXTERNAL_STORAGE at runtime. Rust calls it via JNI through gpui_android::storage::request_once at boot. At targetSdk=28 these are still dangerous permissions requiring runtime prompts.

**Detailed writeup: TODO.** Stub created so the index links resolve.
