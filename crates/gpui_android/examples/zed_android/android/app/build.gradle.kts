import java.net.URL
import java.security.MessageDigest
import java.util.Properties

plugins {
    id("com.android.application")
    kotlin("android")
}

// Release signing config. `signing.properties` and `release.keystore` are
// gitignored: contributors who clone the repo can still build the debug
// variant, but a release build that produces a signed APK requires the
// keystore + properties to be present locally (or provided by CI via
// env-var injected secrets, future work).
val signingPropsFile = file("signing.properties")
val signingProps = Properties().apply {
    if (signingPropsFile.exists()) {
        signingPropsFile.inputStream().use { load(it) }
    }
}
val hasReleaseSigning = signingPropsFile.exists()

// Bootstrap-zip distribution.
//
// `bootstrap-aarch64.zip` is the rebuilt Termux userland (~239 MB) that the
// app extracts into its private data dir at first launch. It is too large
// to ship via git (GitHub's hard cap is 100 MB/file) so it lives as a
// GitHub Release asset instead. Anyone cloning + building gets it auto-
// fetched via the `downloadBootstrap` task below — zero manual steps.
//
// Updating: when you cut a fresh bootstrap (e.g. on a GitHub Actions
// Linux runner running the termux-packages bootstrap-build), bump
// `bootstrapVersion` here AND `BOOTSTRAP_VERSION` in
// `crates/gpui_android/src/termux_bootstrap.rs` to match. The two strings
// have to agree because the Rust side checks them at extract time to
// decide whether to wipe and re-extract $PREFIX.
val bootstrapVersion = "2026.05.06-r2"
val bootstrapDownloadUrl =
    "https://github.com/Dylanmurzello/zed-android-port/releases/download/" +
        "bootstrap-${bootstrapVersion}/bootstrap-aarch64.zip"

val bootstrapAsset = file("src/main/assets/bootstrap-aarch64.zip")
val bootstrapAssetDir = bootstrapAsset.parentFile

tasks.register("downloadBootstrap") {
    description = "Fetch bootstrap-aarch64.zip from the GitHub Release if it's not present locally."
    group = "build setup"

    outputs.file(bootstrapAsset)
    outputs.upToDateWhen { bootstrapAsset.exists() }

    doLast {
        if (bootstrapAsset.exists()) {
            logger.lifecycle("downloadBootstrap: ${bootstrapAsset.name} already present (${bootstrapAsset.length() / 1024 / 1024} MB); skipping download")
            return@doLast
        }
        bootstrapAssetDir.mkdirs()
        logger.lifecycle("downloadBootstrap: fetching bootstrap-${bootstrapVersion} from $bootstrapDownloadUrl")
        URL(bootstrapDownloadUrl).openStream().use { input ->
            bootstrapAsset.outputStream().use { output ->
                input.copyTo(output)
            }
        }
        logger.lifecycle("downloadBootstrap: wrote ${bootstrapAsset.length() / 1024 / 1024} MB to ${bootstrapAsset}")
    }
}

// Make every variant's `mergeAssets` wait on the download. preBuild is
// the simplest hook that runs before assets are read.
tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("downloadBootstrap")
}

// zd-exec bundling.
//
// `zd-exec` is the Rust spawn wrapper (in crates/zdroid_runtime/src/
// bin/zd-exec.rs) the editor invokes as `terminal.shell` for chroot
// mode, and as the symlink target for `$PREFIX/zd-runtime/<name>`
// in chroot+other modes. It MUST be in the APK so fresh installs
// have it. Without bundling, end users hit
// `failed to spawn $PREFIX/bin/zd-exec — no such file or directory`
// the first time they open the integrated terminal in chroot mode.
//
// Build flow:
//   1. `buildZdExec` runs `cargo ndk … build --release -p
//      zdroid_runtime --bin zd-exec` from the workspace root, with
//      $ANDROID_NDK_HOME pointed at the same NDK the lib build uses.
//   2. The resulting ELF at
//      `target/aarch64-linux-android/release/zd-exec` is copied to
//      `app/src/main/assets/zd-exec`.
//   3. `preBuild` depends on it, so gradle picks the asset up during
//      `mergeAssets` before APK packaging.
//
// Rust-side counterpart: `gpui_android::zd_exec_install::ensure_installed`
// reads this asset at boot and extracts to `$PREFIX/bin/zd-exec` with
// 0755 perms when missing or out of date.
val workspaceRoot = file("../../../../../..").canonicalFile
val zdExecBin = file("${workspaceRoot}/target/aarch64-linux-android/release/zd-exec")
val zdExecAsset = file("src/main/assets/zd-exec")
val zdExecSrc = fileTree("${workspaceRoot}/crates/zdroid_runtime/src") {
    include("**/*.rs")
}

tasks.register<Exec>("buildZdExec") {
    description = "Build zd-exec via cargo-ndk and stage into assets/."
    group = "build setup"

    workingDir(workspaceRoot)
    commandLine(
        "cargo",
        "ndk",
        "-t",
        "arm64-v8a",
        "-P",
        "26",
        "build",
        "--release",
        "-p",
        "zdroid_runtime",
        "--bin",
        "zd-exec",
    )

    // Honor whatever the developer's `cargo ndk` lib build uses.
    // ANDROID_NDK_HOME has to be the same NDK or the prebuilts mismatch.
    providers.environmentVariable("ANDROID_NDK_HOME").orNull?.let { ndk ->
        environment("ANDROID_NDK_HOME", ndk)
    }

    inputs.files(zdExecSrc)
    inputs.file("${workspaceRoot}/crates/zdroid_runtime/Cargo.toml")
    outputs.file(zdExecBin)
}

tasks.register<Copy>("stageZdExecAsset") {
    description = "Copy the freshly-built zd-exec into assets/ for APK packaging."
    group = "build setup"

    dependsOn("buildZdExec")
    from(zdExecBin)
    into(zdExecAsset.parentFile)
    rename { zdExecAsset.name }
    // Don't bother re-running on every gradle invocation when the
    // source binary hasn't changed.
    inputs.file(zdExecBin)
    outputs.file(zdExecAsset)
}

tasks.matching { it.name == "preBuild" }.configureEach {
    dependsOn("stageZdExecAsset")
}

android {
    namespace = "com.zdroid"
    compileSdk = 35

    // Pin the NDK explicitly so reproducibility doesn't depend on whatever
    // `sdkmanager --list_installed` happens to surface. Bionic's
    // `forkpty()` is in API 23+, so any NDK ≥ r21 is sufficient; we use r27
    // because that's the one we shipped L1 with and `+fp16` codegen
    // (gemm-f16) wants a recent toolchain.
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "com.zdroid"
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

    // `targetSdk = 28` is intentional (see the comment in defaultConfig). It
    // pins us in the SELinux `untrusted_app_27` domain so the bundled Termux
    // runtime can `execve` $PREFIX/bin/*. AGP's `lintVitalRelease` task
    // flags this as `ExpiredTargetSdkVersion` and refuses to assemble the
    // release APK. We're not Play-Store eligible by design — disable that
    // single rule rather than bumping the SDK and breaking exec.
    lint {
        disable += "ExpiredTargetSdkVersion"
    }

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(signingProps.getProperty("storeFile"))
                storePassword = signingProps.getProperty("storePassword")
                keyAlias = signingProps.getProperty("keyAlias")
                keyPassword = signingProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = false
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            }
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
