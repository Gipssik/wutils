fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("slint compile failed");

    // App icon + the manifest that declares per-monitor-v2 DPI awareness and
    // the Segoe Fluent Icons dependency. Only on the final binary.
    embed_resource::compile("wutils.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
