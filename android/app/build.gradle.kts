plugins { id("com.android.application") }
android {
    namespace = "org.poknite"
    compileSdk = 37
    defaultConfig {
        applicationId = "org.poknite"
        minSdk = 26
        targetSdk = 37
        versionCode = 2
        versionName = "0.2.0"
        testInstrumentationRunner = "org.poknite.TestRunner"
    }
    buildFeatures { buildConfig = true }
    sourceSets.getByName("androidTest").assets.srcDir("../../tools/fixtures")
    buildTypes {
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    packaging { resources.excludes += setOf("META-INF/LICENSE*", "META-INF/NOTICE*", "META-INF/*.kotlin_module") }
}
dependencies {
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation("junit:junit:4.13.2")
    // The old platform test runner is an optional SDK library, only on the test classpath.
    androidTestCompileOnly(files(androidComponents.sdkComponents.sdkDirectory.map { it.file("platforms/android-37.0/optional/android.test.runner.jar") }))
    androidTestCompileOnly(files(androidComponents.sdkComponents.sdkDirectory.map { it.file("platforms/android-37.0/optional/android.test.base.jar") }))
}
