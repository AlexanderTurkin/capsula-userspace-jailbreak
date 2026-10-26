//! capsula-jb — jailbreak CLI for VK Capsula speakers (Rockchip RK3308).
//!
//! Method: userspace overwrite. The userdata partition is not signed and
//! can be rewritten by the user freely; no other partition is modified.
//!
//! Default scenario (no subcommand):
//!   1. fetch the Capsule-14 OTA, extract the loader into ota/ (cached;
//!      one loader for all supported boards)
//!   2. wait for the device in rockchip loader mode, start our usbplug
//!      in RAM
//!   3. patch the read limit over USB (0xCC bypass), dump all partitions
//!      into backup/
//!   4. build the adbd overlay on top of the userdata dump
//!   5. flash userdata back
//!   6. reboot and wait for the ADB gadget (2207:0006) to come up

mod progress;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use jb_core::backup_store::BackupStore;
use jb_core::config::Config;
use jb_core::session::{bring_up, BringUpError, Progress};
use jb_core::{backup, flash, ota, userdata};
use rockusb::loader::RESET_SUBCODE_NORMAL;
use rockusb::{LoaderContainer, NusbTransport};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The gadget must stay on the bus this long to be considered alive:
/// the stock firmware blinks 0006 for under a second.
const GADGET_STABLE: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(
    name = "capsula-jb",
    version,
    about = "VK Capsula (Rockchip RK3308) userspace jailbreak: dump -> adbd overlay in userdata -> ADB"
)]
struct Cli {
    /// Timeout for waiting for the device on USB, seconds (0 = forever)
    #[arg(long, value_name = "SEC", default_value = "0")]
    wait: u64,

    /// Do not ask for confirmation before writing userdata (scripts)
    #[arg(long)]
    yes: bool,

    /// Format userdata as a clean ext2 instead of patching the dump
    /// (existing userdata content is lost)
    #[arg(long)]
    format_userdata: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Dump all partitions into backup/ only (nothing is written to the device)
    Backup,
    /// Live log of 2207:* USB events
    Monitor,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let wait = if cli.wait == 0 {
        None
    } else {
        Some(Duration::from_secs(cli.wait))
    };
    match cli.command {
        None => run_jailbreak(wait, cli.yes, cli.format_userdata),
        Some(Command::Backup) => run_backup(wait),
        Some(Command::Monitor) => monitor(),
    }
}

fn print_step(ev: Progress) {
    if let Progress::Step(msg) = ev {
        println!("{msg}");
    }
}

/// Prepare the loader: ota/loader.bin from cache or a fresh OTA download.
fn ensure_loader(config: &Config) -> anyhow::Result<LoaderContainer> {
    let (container, version) =
        ota::ensure_loader(Path::new("ota"), &config.ota.platform, &|step| match step {
            ota::Step::Cached(path) => println!("loader: using cached {path}"),
            ota::Step::Fetching(code) => println!("querying latest firmware for {code}..."),
            ota::Step::Unpacking(path) => println!("extracting loader from OTA package {path}"),
            ota::Step::Saved { path, version } => {
                println!("loader saved: {path} (OTA {version})")
            }
        })
        .context("failed to prepare the loader from the OTA")?;
    if let Some(v) = version {
        println!("loader OTA version: {v}");
    }
    Ok(container)
}

/// Steps 1-3: loader, device, full dump into backup/. Returns the live
/// usbplug session (usable for the subsequent write).
fn dump_all(wait: Option<Duration>) -> anyhow::Result<rockusb::LoaderSession<NusbTransport>> {
    let config = Config::embedded();
    let loader = ensure_loader(&config)?;

    let p = progress::CliProgress::new();
    let progress = |ev: Progress| p.handle(ev);
    let mut s = match bring_up(&config, &loader, wait, &progress) {
        Ok(s) => s,
        Err(BringUpError::LoaderRejected) => bail!(
            "usbplug did not start: BootROM rejected the loader signature (unsupported board?)"
        ),
        Err(e) => return Err(e.into()),
    };
    backup::backup(&mut s, &config, Path::new("backup"), &progress)?;
    Ok(s)
}

/// `capsula-jb backup`: dump only.
fn run_backup(wait: Option<Duration>) -> anyhow::Result<()> {
    let _session = dump_all(wait)?;
    println!("done: dumps are in backup/ (nothing was written to the device)");
    Ok(())
}

/// Full jailbreak scenario (steps 1-6).
fn run_jailbreak(wait: Option<Duration>, yes: bool, format_userdata: bool) -> anyhow::Result<()> {
    let mut s = dump_all(wait)?;

    // step 4: overlay on top of the userdata dump
    let store = BackupStore::open(Path::new("backup")).context("no valid backup in backup/")?;
    store
        .verify()
        .context("backup failed SHA256 verification — run backup again")?;
    let dump = store
        .find("userdata")
        .context("no userdata partition in the backup")?
        .to_path_buf();
    let part_size = std::fs::metadata(&dump)?.len();

    if !yes {
        println!(
            "about to write userdata ({}): {}",
            humansize(part_size),
            if format_userdata {
                "adbd overlay on a clean ext2 (--format-userdata, existing content is lost)"
            } else {
                "adbd overlay on top of the dump, existing content is preserved"
            }
        );
        print!("proceed? [y/N] ");
        ask_yes()?;
    }

    let marker = format!("capsula-jb userspace {}\n", utc_now_iso());
    let out = PathBuf::from("work").join("userdata.img");
    userdata::build_image(
        &dump,
        &out,
        format_userdata,
        part_size,
        &userdata::overlay_files(marker),
        &print_step,
    )
    .context("failed to build the userdata image with the overlay")?;

    // step 5: flash the partition back (same usbplug session)
    let p = progress::CliProgress::new();
    let progress = |ev: Progress| p.handle(ev);
    let gpt = jb_core::session::read_gpt(&mut s, &progress)?;
    flash::write_partition_padded(&mut s, &gpt, "userdata", &out, &progress)?;

    // step 6: reboot and wait for ADB
    println!("rebooting the device...");
    let _ = s.reset(RESET_SUBCODE_NORMAL);
    println!(
        "waiting for the ADB gadget (2207:0006, {} s stability window)...",
        GADGET_STABLE.as_secs()
    );
    rockusb::monitor::wait_gadget_stable(GADGET_STABLE, wait).context(
        "ADB gadget did not come up (or disappeared after a blink): check the device boot",
    )?;
    println!("done: ADB is up. The device accepts root shell connections.");
    Ok(())
}

fn ask_yes() -> anyhow::Result<()> {
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    let ans = ans.trim().to_lowercase();
    if ans != "y" && ans != "yes" {
        bail!("aborted by user");
    }
    Ok(())
}

/// Live log of 2207:* bus events.
fn monitor() -> anyhow::Result<()> {
    let mut m = rockusb::DeviceMonitor::new();
    println!("Rockchip USB monitor (2207:*), Ctrl-C to stop.");
    m.poll()?;
    if let Some(best) = rockusb::transport::DeviceMode::best(m.modes()) {
        println!("on the bus now: {}", best.label());
    }
    loop {
        for ev in m.poll()? {
            match ev {
                rockusb::monitor::MonitorEvent::Appeared(mode) => {
                    println!(
                        "[{}] appeared 2207:{:04x} ({})",
                        ts(),
                        mode.pid(),
                        mode.label()
                    )
                }
                rockusb::monitor::MonitorEvent::Disappeared(mode) => {
                    println!(
                        "[{}] gone     2207:{:04x} ({})",
                        ts(),
                        mode.pid(),
                        mode.label()
                    )
                }
            }
        }
        std::thread::sleep(rockusb::monitor::POLL_INTERVAL);
    }
}

fn ts() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs() % 86400;
            format!(
                "{:02}:{:02}:{:02}",
                secs / 3600,
                (secs / 60) % 60,
                secs % 60
            )
        })
        .unwrap_or_default()
}

fn humansize(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Current UTC time as 2026-10-26T12:34:56Z.
fn utc_now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
