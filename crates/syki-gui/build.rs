fn main() {
    println!("cargo:rerun-if-changed=../../app.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../app.ico");
    if let Err(err) = res.compile() {
        panic!("failed to compile Windows icon resource: {err}");
    }
}
