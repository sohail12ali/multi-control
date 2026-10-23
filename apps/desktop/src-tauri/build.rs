fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["daemon_request", "set_expanded"]),
        ),
    )
    .expect("failed to build the Multi Control panel");
}
