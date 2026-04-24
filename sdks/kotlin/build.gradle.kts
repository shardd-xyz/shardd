import com.vanniktech.maven.publish.JavadocJar
import com.vanniktech.maven.publish.KotlinJvm
import com.vanniktech.maven.publish.SonatypeHost
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm") version "2.0.21"
    id("com.vanniktech.maven.publish") version "0.30.0"
}

group = "xyz.shardd"
version = "0.1.0"

repositories {
    mavenCentral()
}

java {
    sourceCompatibility = JavaVersion.VERSION_11
    targetCompatibility = JavaVersion.VERSION_11
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_11)
        freeCompilerArgs.add("-Xjsr305=strict")
    }
}

dependencies {
    implementation("com.fasterxml.jackson.core:jackson-databind:2.18.2")
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin:2.18.2")

    testImplementation(kotlin("test"))
    testImplementation("org.junit.jupiter:junit-jupiter:5.11.4")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
}

tasks.test {
    useJUnitPlatform()
}

tasks.jar {
    manifest {
        attributes["Automatic-Module-Name"] = "xyz.shardd.sdk"
    }
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL)
    // Gated so ./gradlew publishToMavenLocal works without GPG credentials.
    if (providers.gradleProperty("signingInMemoryKey").isPresent) {
        signAllPublications()
    }

    coordinates("xyz.shardd", "sdk", project.version.toString())

    configure(KotlinJvm(javadocJar = JavadocJar.Empty(), sourcesJar = true))

    pom {
        name.set("shardd Kotlin SDK")
        description.set("Official Kotlin/JVM client for shardd.")
        url.set("https://github.com/shardd-xyz/shardd/tree/main/sdks/kotlin")
        inceptionYear.set("2026")

        licenses {
            license {
                name.set("MIT License")
                url.set("https://opensource.org/licenses/MIT")
                distribution.set("repo")
            }
        }

        developers {
            developer {
                id.set("shardd")
                name.set("shardd maintainers")
                url.set("https://github.com/shardd-xyz")
            }
        }

        scm {
            url.set("https://github.com/shardd-xyz/shardd")
            connection.set("scm:git:https://github.com/shardd-xyz/shardd.git")
            developerConnection.set("scm:git:ssh://git@github.com/shardd-xyz/shardd.git")
        }
    }
}
