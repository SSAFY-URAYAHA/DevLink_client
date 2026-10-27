use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub fn diag(tag: &str, msg: &str) {
    let start = START.get_or_init(Instant::now);
    let elapsed = start.elapsed().as_millis();
    let pid = std::process::id();
    let line = format!("[+{:>5}ms | PID {:>5}] [{}] {}", elapsed, pid, tag, msg);
    eprintln!("{}", line);
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("devlink_diag.log"))
    {
        let _ = writeln!(f, "{}", line);
        let _ = f.flush();
    }
}
