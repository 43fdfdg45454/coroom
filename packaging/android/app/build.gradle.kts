plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

// Versión X.Y.Z: la de GitVersion que pasa la CI (-Pcoroom.version=X.Y.Z), o 1.0.0 en local.
val coroomVersion: String = providers.gradleProperty("coroom.version").getOrElse("1.0.0")
val coroomVersionCode: Int = run {
    val parts = coroomVersion.substringBefore('-').split('.').map { it.toIntOrNull() }
    require(parts.size == 3 && parts.all { it != null && it in 0..999 }) {
        "coroom.version tiene que ser X.Y.Z (cada parte de 0 a 999): $coroomVersion"
    }
    val (major, minor, patch) = parts.map { it!! }
    major * 1_000_000 + minor * 1_000 + patch
}

// Firma de la release: el almacén y sus contraseñas llegan por el entorno (secretos de la CI);
// sin ellos, la release se firma con la clave de depuración para poder instalarla y probarla.
val releaseKeystore = providers.environmentVariable("COROOM_ANDROID_KEYSTORE").orNull
val releaseKeystorePassword = providers.environmentVariable("COROOM_ANDROID_KEYSTORE_PASSWORD").orNull
val releaseKeyAlias = providers.environmentVariable("COROOM_ANDROID_KEY_ALIAS").orNull
val releaseKeyPassword = providers.environmentVariable("COROOM_ANDROID_KEY_PASSWORD").orNull
val hasReleaseKey = listOf(releaseKeystore, releaseKeystorePassword, releaseKeyAlias, releaseKeyPassword)
    .all { !it.isNullOrBlank() }

android {
    namespace = "coroom.android"
    compileSdk = 37

    defaultConfig {
        applicationId = "coroom.android"
        // MediaCodec asíncrono con ANativeWindow y AAudio, que usa la parte en Rust, piden API 28.
        minSdk = 28
        targetSdk = 37
        versionCode = coroomVersionCode
        versionName = coroomVersion
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        if (hasReleaseKey) {
            create("release") {
                storeFile = file(releaseKeystore!!)
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            signingConfig = signingConfigs.getByName(if (hasReleaseKey) "release" else "debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        // Las .so van sin comprimir y alineadas dentro del APK: se cargan sin extraerlas.
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

// La biblioteca en Rust (`apps/android`) para los dos ABI. La CI la compila en un paso aparte; en
// local, `-Pcoroom.cargo=true` la compila antes de cada build (hace falta cargo-ndk y el NDK).
val repoRoot = rootProject.layout.projectDirectory.dir("../..")
val cargoBuild by tasks.registering(Exec::class) {
    group = "build"
    description = "Compila libcoroom_android.so con cargo-ndk para arm64-v8a y x86_64."
    workingDir(repoRoot)
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-t", "x86_64",
        "-P", "28",
        "-o", "packaging/android/app/src/main/jniLibs",
        "build", "--release", "-p", "coroom-android",
    )
}
if (providers.gradleProperty("coroom.cargo").map { it.toBoolean() }.getOrElse(false)) {
    tasks.named("preBuild") { dependsOn(cargoBuild) }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.androidx.core)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.datastore.preferences)
    implementation(libs.kotlinx.serialization.json)
}
