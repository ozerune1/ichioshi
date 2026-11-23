import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release builds are signed with this computer's own key (the one the
// earlier builds used, so this installs over them), when it's there.
val seadexKey = file(System.getProperty("user.home") + "/.local/share/seadex-android/seadex.keystore")

android {
    compileSdk = 37
    signingConfigs {
        create("seadex") {
            if (seadexKey.exists()) {
                storeFile = seadexKey
                storePassword = "seadex-android"
                keyAlias = "seadex"
                keyPassword = "seadex-android"
            }
        }
    }
    namespace = "app.seadex.downloader"
    defaultConfig {
        // Servers at home are often plain HTTP.
        manifestPlaceholders["usesCleartextTraffic"] = "true"
        applicationId = "app.seadex.downloader"
        // Android 11: the library goes in the Download folder (scoped
        // storage, no permission needed).
        minSdk = 30
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (seadexKey.exists()) {
                signingConfig = signingConfigs.getByName("seadex")
            }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        buildConfig = true
    }
    packaging {
        // libmpv brings its own C++ runtime too.
        jniLibs.pickFirsts.add("**/libc++_shared.so")
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    // The player: mpv's engine, for Android.
    implementation("dev.jdtech.mpv:libmpv:1.1.0")
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
