plugins {
    id("com.android.library")
}

// Native library + UniFFI Kotlin bindings, produced by ../../scripts/build-sdk-android.sh into dist/android.
val dist = rootDir.resolve("../dist/android")

android {
    namespace = "dev.osmo.sdk"
    compileSdk = 37
    defaultConfig { minSdk = 26 }
    sourceSets {
        getByName("main") {
            kotlin.directories.add(dist.resolve("kotlin").path)
            jniLibs.directories.add(dist.resolve("jniLibs").path)
        }
    }
}

dependencies {
    api("net.java.dev.jna:jna:5.14.0@aar")
}
