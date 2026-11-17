fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "desktop_status",
            "local_link_status",
            "begin_local_link_login",
            "poll_local_link_login",
            "sign_out_local_link",
            "connect_local",
            "begin_link_login",
            "poll_link_login",
            "list_machines",
            "connect_link_machine",
            "disconnect_session",
            "test_notification",
            "diagnostics",
            "export_diagnostics",
            "sign_out",
            "check_updates",
            "install_update",
            "saved_hubs",
            "list_downloads",
            "close_settings",
            "updater_configured",
        ]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
