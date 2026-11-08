plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

// dist/android is produced by scripts/build-sdk-android.sh
val dist = rootProject.projectDir.resolve("../../dist/android")

android {
    namespace = "dev.osmo.sdk"
    compileSdk = 36
    defaultConfig {
        minSdk = 24
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    sourceSets {
        getByName("main") {
            java.srcDir(dist.resolve("kotlin"))
            jniLibs.srcDir(dist.resolve("jniLibs"))
        }
        getByName("androidTest") {
            assets.srcDir(rootProject.projectDir.resolve("../fixtures"))
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
}
