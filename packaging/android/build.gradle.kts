// AGP 9 compila Kotlin él mismo («built-in Kotlin») y trae su propio Kotlin Gradle plugin; se pone
// en el classpath el fijado en el catálogo para que el compilador, el plugin de Compose y el de
// serialización vayan con la misma versión.
buildscript {
    dependencies {
        classpath(libs.kotlin.gradle.plugin)
    }
}

plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
}
