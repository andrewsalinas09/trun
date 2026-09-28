// The web UI is embedded from ui/dist. A plain `cargo build` must work without
// Node, so create a placeholder page when the UI hasn't been built yet.
fn main() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create ui/dist");
        std::fs::write(
            &index,
            "<!doctype html><meta charset=utf-8><title>trun</title>\
             <body style=\"font-family:system-ui;padding:2rem\">\
             <h1>trun</h1><p>The web UI has not been built. Run <code>npm run build</code> in <code>ui/</code> \
             and rebuild trun. The API is available under <code>/api</code>.</p>",
        )
        .expect("write placeholder index.html");
    }
    println!("cargo:rerun-if-changed=../../ui/dist");
}
