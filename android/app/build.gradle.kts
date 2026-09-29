plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.dmsg.client"
    compileSdk = 37
    // Only 37.0.0 is installed in this environment.
    buildToolsVersion = "37.0.0"

    defaultConfig {
        applicationId = "org.dmsg.client"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.4.0-k4"
        // Budgets (ARCH/WORK_PLAN M4): release APK <= 25 MiB, idle PSS <= 100 MiB.
        // Measured on device in the gate checklist; not promised here.
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        getByName("debug") {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    packaging {
        // Native libdmsg_core.so is built with cargo-ndk when an NDK is present
        // (see docs gate checklist). Debug APK without it still assembles; the
        // app reports "core missing" instead of crashing (see Core.kt).
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

dependencies {
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.11.0")
    implementation("androidx.core:core-ktx:1.16.0")
    // UniFFI runtime for generated bindings (uniffi/dmsg_core/dmsg_core.kt).
    implementation("net.java.dev.jna:jna:5.13.0")
    // QR: show contact-QR + offline scan (both dmsg:// formats).
    implementation("com.google.zxing:core:3.5.4")
    implementation("androidx.camera:camera-core:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")
    // Keystore-wrap master key + EncryptedFile sealing (see SecureStore.kt).
    implementation("androidx.security:security-crypto:1.0.0")

    testImplementation("junit:junit:4.13.2")
}
