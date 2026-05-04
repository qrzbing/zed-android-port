# Debug-strip oversized .so

**Status:** Active

Debug-build libzed_android.so blew past 2 GB once client + project + workspace + reqwest_client joined the dep graph. llvm-strip can't parse ELF section tables that large and silently packages the unstripped binary, which then fails dlopen with 'dynamic section header not found'. Cargo.toml [profile.dev] sets debug = 'line-tables-only' and strip = 'debuginfo'.

**Detailed writeup: TODO.** Stub created so the index links resolve.
