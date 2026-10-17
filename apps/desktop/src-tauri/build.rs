fn main() {
    // Enroll app commands in the ACL. Without AppManifest, app commands are
    // available to every local webview regardless of plugin capabilities.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "connection_info",
                "list_operations",
                "get_operation",
                "read_output",
                "cancel_operation",
            ]),
        ),
    ).expect("could not build Devagotchi application permissions");
}
