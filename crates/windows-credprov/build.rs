// Embed the hourglass icon (resource 1) for the tile image.
fn main() {
    println!("cargo:rerun-if-changed=../windows/assets/app.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("../windows/assets/app.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
