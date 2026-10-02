use nv_core::{apps::AppIndex, Config};
fn main() {
    let cfg = Config::default();
    let t = std::time::Instant::now();
    let idx = AppIndex::scan(&cfg); idx.save_cache();
    println!("{} apps in {:?}", idx.apps.len(), t.elapsed());
    let with_exe = idx.apps.iter().filter(|a| a.exe.is_some()).count();
    println!("{with_exe} with exe");
    for a in idx.apps.iter().take(15) { println!("  {:?}", a); }
    for q in ["chrome", "vs code", "code", "spotify", "discord", "steam", "calculator", "settings", "word", "file explorer", "task manager", "notepad", "terminal", "edge"] {
        println!("{q:15} -> {:?}", idx.find(q, &cfg).map(|(a, s)| (a.name, (s*100.0) as i32)));
    }
}
