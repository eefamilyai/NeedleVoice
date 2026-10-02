fn main() {
    let id = std::env::args().nth(1).unwrap();
    let mut last = 0;
    let r = nv_core::voices::download(&id, |d, t| { let pct = if t > 0 { d * 100 / t } else { 0 }; if pct >= last + 25 { last = pct; println!("{pct}%"); } });
    println!("{r:?} installed={}", nv_core::voices::is_installed(&id));
}
