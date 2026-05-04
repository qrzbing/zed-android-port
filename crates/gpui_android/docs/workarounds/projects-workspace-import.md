# ~/projects workspace + Import-from-sdcard flow

**Status:** Active

~/projects is the canonical workspace dir, mkdir'd at boot. File menu has Import-from-sdcard which fires the SAF picker, recursively copies the picked tree to ~/projects/<basename>, opens the local copy. Original on /sdcard untouched. Implements via the ImportFromSdcard action in lib.rs and storage::copy_tree.

**Detailed writeup: TODO.** Stub created so the index links resolve.
