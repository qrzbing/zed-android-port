package dev.zed.zed_android

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.util.Log
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
    }
}
