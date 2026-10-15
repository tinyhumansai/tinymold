rootProject.name = "tinymold-gradle-fixture"

buildCache {
    local {
        isEnabled = false
    }
    remote<HttpBuildCache> {
        url = uri(System.getenv("TINYMOLD_GRADLE_CACHE"))
        isPush = true
        isAllowInsecureProtocol = true
        credentials {
            username = "gradle"
            password = System.getenv("TINYMOLD_GRADLE_TOKEN")
        }
    }
}
