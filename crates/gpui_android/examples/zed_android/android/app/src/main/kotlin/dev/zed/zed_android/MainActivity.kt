package dev.zed.zed_android

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.util.Log
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.google.androidgamesdk.GameActivity

/// SAF flows go through legacy `startActivityForResult` instead of
/// `ActivityResultLauncher` because `ActivityResultRegistry` silently
/// no-ops `launch()` when the host is in a non-STARTED lifecycle state,
/// which is the typical case when the call comes from a JNI thread driven
/// by gpui's render loop. AGDK's own SAF samples use the legacy path for
/// the same reason — `GameActivity` forwards `onActivityResult` correctly
/// to its Java host, and we get the result without any of the registry
/// gating.
class MainActivity : GameActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
    }

    @Suppress("unused") // called from Rust via JNI
    fun launchOpenTree() {
        Log.i(TAG, "launchOpenTree() invoked")
        runOnUiThread {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
                // Suggest the primary external storage root so the picker
                // lands somewhere familiar instead of "Recent".
                putExtra(
                    DocumentsContract.EXTRA_INITIAL_URI,
                    DocumentsContract.buildRootUri(
                        "com.android.externalstorage.documents",
                        "primary"
                    )
                )
            }
            try {
                startActivityForResult(intent, REQ_OPEN_TREE)
                Log.i(TAG, "startActivityForResult OPEN_DOCUMENT_TREE dispatched")
            } catch (t: Throwable) {
                Log.e(TAG, "OPEN_DOCUMENT_TREE dispatch threw", t)
                onPickerResult("")
            }
        }
    }

    @Suppress("unused") // called from Rust via JNI
    fun launchCreateDocument(suggestedName: String) {
        Log.i(TAG, "launchCreateDocument($suggestedName) invoked")
        runOnUiThread {
            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "application/octet-stream"
                putExtra(Intent.EXTRA_TITLE, suggestedName)
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                )
            }
            try {
                startActivityForResult(intent, REQ_CREATE_DOCUMENT)
                Log.i(TAG, "startActivityForResult CREATE_DOCUMENT dispatched")
            } catch (t: Throwable) {
                Log.e(TAG, "CREATE_DOCUMENT dispatch threw", t)
                onPickerResult("")
            }
        }
    }

    /// Returns 1 if both READ + WRITE are already granted, 0 if a runtime
    /// dialog has been posted. Caller fires this once on boot and treats
    /// the call as best-effort: if the user denies, file-system reads of
    /// `/storage/emulated/0/...` will EACCES at the syscall layer with a
    /// clean error.
    @Suppress("unused") // called from Rust via JNI
    fun requestStoragePermissions(): Int {
        val needed = listOf(
            Manifest.permission.READ_EXTERNAL_STORAGE,
            Manifest.permission.WRITE_EXTERNAL_STORAGE,
        ).filter {
            ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
        }
        if (needed.isEmpty()) {
            Log.i(TAG, "requestStoragePermissions: already granted")
            return 1
        }
        Log.i(TAG, "requestStoragePermissions: prompting for ${needed.joinToString(",")}")
        runOnUiThread {
            ActivityCompat.requestPermissions(this, needed.toTypedArray(), REQ_STORAGE_PERMS)
        }
        return 0
    }

    /// Returns Android's currently-active DNS server IPs as a comma-joined
    /// string. The Rust side writes them to /sdcard/.zed/r in resolv.conf
    /// format so Bun-compiled CLIs (whose c-ares is patched to read from
    /// /sdcard/.zed/r) can do DNS without proot. Falls back to empty
    /// string if no active network — caller layers in public-DNS defaults.
    @Suppress("unused") // called from Rust via JNI
    fun getActiveDnsServers(): String {
        val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return ""
        val network = cm.activeNetwork ?: return ""
        val props = cm.getLinkProperties(network) ?: return ""
        return props.dnsServers
            .mapNotNull { it.hostAddress }
            .joinToString(",")
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQ_STORAGE_PERMS) {
            return
        }
        val results = permissions.zip(grantResults.toTypedArray()).joinToString(",") { (perm, granted) ->
            "${perm.removePrefix("android.permission.")}=${if (granted == PackageManager.PERMISSION_GRANTED) "OK" else "DENIED"}"
        }
        Log.i(TAG, "onRequestPermissionsResult: $results")
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_OPEN_TREE && requestCode != REQ_CREATE_DOCUMENT) {
            return
        }
        if (resultCode != Activity.RESULT_OK) {
            Log.i(TAG, "picker cancelled (req=$requestCode resultCode=$resultCode)")
            onPickerResult("")
            return
        }
        val uri: Uri? = data?.data
        if (uri != null) {
            try {
                contentResolver.takePersistableUriPermission(
                    uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                )
            } catch (t: Throwable) {
                Log.w(TAG, "takePersistableUriPermission failed", t)
            }
        }
        onPickerResult(uri?.toString() ?: "")
    }

    private external fun onPickerResult(uriString: String)

    companion object {
        private const val TAG = "zed_android_saf"
        private const val REQ_OPEN_TREE = 0xA1
        private const val REQ_CREATE_DOCUMENT = 0xA2
        private const val REQ_STORAGE_PERMS = 0xA3
    }
}
