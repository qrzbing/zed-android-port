import groovy.json.JsonSlurper

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

// Resolve the bundled maven dir shipped inside the
// `rustls-platform-verifier-android` cargo crate. The Android verifier
// component is published as an `.aar` inside `$CARGO_HOME/registry/src/
// .../rustls-platform-verifier-android-<ver>/maven`; cargo manages the
// version, gradle just needs the on-disk path. Without this the `.aar`
// can't be located and Android rustls TLS handshakes fail with
// `UnknownIssuer`. See `crates/gpui_android/examples/zed_android/src/
// lib.rs::init_platform_tls` for the matching Rust-side init.
fun findRustlsPlatformVerifierMaven(): String {
    // zed_android example has its own [workspace] Cargo.toml one dir up
    // from `android/`. That's the manifest whose dep tree gradle needs to
    // walk, not the repo-root workspace.
    val manifest = file("../Cargo.toml").absolutePath
    val out = providers.exec {
        commandLine("cargo", "metadata", "--format-version", "1",
            "--manifest-path", manifest)
    }.standardOutput.asText.get()
    val json = JsonSlurper().parseText(out) as Map<*, *>
    val packages = json["packages"] as List<*>
    val pkg = packages.map { it as Map<*, *> }
        .first { it["name"] == "rustls-platform-verifier-android" }
    val manifestPath = file(pkg["manifest_path"] as String)
    return File(manifestPath.parentFile, "maven").absolutePath
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        maven {
            // Read the bundled POM directly; it declares
            // `<packaging>aar</packaging>` which gradle needs to know to
            // resolve `rustls-platform-verifier-0.1.1.aar` instead of
            // looking for a `.jar`.
            url = uri(findRustlsPlatformVerifierMaven())
        }
    }
}

rootProject.name = "zed_android"
include(":app")
