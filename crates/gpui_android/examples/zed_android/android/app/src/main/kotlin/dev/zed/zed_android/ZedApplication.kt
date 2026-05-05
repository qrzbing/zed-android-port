package dev.zed.zed_android

import android.app.Application

/// Loads the native library before any Activity starts. Required because
/// `ExtraWindowActivity` extends `AppCompatActivity` and does not trigger
/// the GameActivity meta-data path that loads `libzed_android.so`. With the
/// load centralized here, both activities can call into JNI on first touch
/// without per-Activity init blocks racing against Android's class-loader
/// when an Activity is recreated.
class ZedApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        System.loadLibrary("zed_android")
    }
}
