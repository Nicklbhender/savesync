import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

android {
    namespace = "com.nicklbhender.savesync"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.nicklbhender.savesync"
        minSdk = 26
        targetSdk = 37
        // Version comes from -PsavesyncVersion (set by scripts/release.sh); versionCode is derived.
        val version = (project.findProperty("savesyncVersion") as String?) ?: "1.0.2"
        val (major, minor, patch) = version.split(".").map { it.toInt() }
        versionCode = major * 10000 + minor * 100 + patch
        versionName = version
        // The Rust engine is built for 64-bit ARM (every current phone, and the Android emulator on ARM-based Macs).
        ndk { abiFilters += "arm64-v8a" }
        // Where to look for updates; overridable for testing with -PsavesyncUpdateApi=...
        val updateApi = (project.findProperty("savesyncUpdateApi") as String?)
            ?: "https://api.github.com/repos/Nicklbhender/savesync/releases/latest"
        buildConfigField("String", "UPDATE_API", "\"$updateApi\"")
    }

    // Release signing key lives outside the repo: a properties file (storeFile,
    // storePassword, keyAlias, keyPassword) at $SAVESYNC_ANDROID_SIGNING or
    // ~/.savesync/android-release.properties. Back it up: updates to installed apps
    // must be signed with the same key.
    val signingProps = System.getenv("SAVESYNC_ANDROID_SIGNING")?.let { File(it) }
        ?: File(System.getProperty("user.home"), ".savesync/android-release.properties")
    signingConfigs {
        if (signingProps.exists()) {
            val p = Properties().apply { signingProps.inputStream().use { load(it) } }
            create("release") {
                storeFile = File(p.getProperty("storeFile"))
                storePassword = p.getProperty("storePassword")
                keyAlias = p.getProperty("keyAlias")
                keyPassword = p.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    // UniFFI's generated bindings call into the Rust library through JNA.
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    // Push via the user's own ntfy server (any UnifiedPush distributor works).
    implementation("org.unifiedpush.android:connector:3.3.5")
}
