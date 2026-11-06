//! Embeds the application icon into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=shcp.rc");
    println!("cargo:rerun-if-changed=assets/shcp.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && let Err(error) =
            embed_resource::compile("shcp.rc", embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=Could not embed the app icon: {error}");
    }
}
