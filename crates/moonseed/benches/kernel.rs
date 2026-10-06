fn main() {
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    let cpu = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = cpu
        .lines()
        .find(|line| line.starts_with("model name"))
        .unwrap_or("model name: unknown");
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    println!("moonseed measurements");
    println!(
        "profile=bench debug_assertions={} os={} arch={}",
        cfg!(debug_assertions),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("rustc {}", rustc.trim());
    println!("{model}");
    println!("commit {}", commit.trim());
    println!("warmup=1 timed_runs=20 median; rewind is outside the timer");
    for sample in moonseed::measure() {
        println!(
            "{:<24} {:>8} ns/op  {}",
            sample.name, sample.ns_per_op, sample.detail
        );
    }
}
