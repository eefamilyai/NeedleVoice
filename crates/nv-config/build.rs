fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("../../assets/icon.ico", "1");
    res.set("FileDescription", "NeedleVoice Settings");
    res.set("ProductName", "NeedleVoice");
    res.compile().expect("embedding icon");
}
