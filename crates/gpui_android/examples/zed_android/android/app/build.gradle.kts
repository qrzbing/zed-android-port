plugins {
    id("com.android.application")
    kotlin("android")
}

android {
    namespace = "dev.zed.zed_android"
    compileSdk = 35

    // Pin the NDK explicitly so reproducibility doesn't depend on whatever
    // `sdkmanager --list_installed` happens to surface. Bionic's
    // `forkpty()` is in API 23+, so any NDK ≥ r21 is sufficient; we use r27
    // because that's the one we shipped L1 with and `+fp16` codegen
    // (gemm-f16) wants a recent toolchain.
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "dev.zed.zed_android"
        // minSdk = 26 enforces bionic ≥ Oreo. `forkpty()` is on the symbol
        // table from API 23, but cpal/livekit transitive crates require
        // libaaudio which is API 26.
        minSdk = 26
        // targetSdk = 28 is the linchpin of the bundled Termux runtime:
        // SELinux puts us in the `untrusted_app_27` domain where
        // `execute_no_trans` on `app_data_file` is permitted, so we can
        // execve $PREFIX/bin/* directly. Pinning > 28 lands in
        // `untrusted_app_all` / numbered higher domains where exec is
        // denied — the entire L2 plan stops working. Skipping Play Store
        // eligibility is the explicit trade.
        targetSdk = 28
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    // Don't deflate bootstrap-aarch64.zip during APK packaging — it's
    // already a deflated zip, and re-deflating it (a) wastes APK size
    // (b) forces AAssetManager to decompress at runtime, which prevents
    // the bootstrap extractor from using the mmap-able buffer path.
    androidResources {
        noCompress += listOf("zip")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = false
        }
    }
}

dependencies {
    implementation("androidx.games:games-activity:3.0.5")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core-ktx:1.13.1")
    // ActivityResultLauncher / ActivityResultContracts for SAF picker.
    implementation("androidx.activity:activity-ktx:1.9.3")
}
