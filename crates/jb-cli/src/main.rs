//! capsula-jb — jailbreak CLI for VK Capsula speakers.
//!
//! Method: userspace overwrite (Rockchip boards). The userdata partition
//! is not signed and can be rewritten by the user freely; no other
//! partition is modified.
//!
//! Default scenario (no subcommand):
//!   0. wait for a supported speaker to be plugged in and powered, then
//!      dispatch on the SoC vendor:
//!      - Rockchip (2207:330d/330e): the full scenario below;
//!      - Amlogic (1b8e:c003, the late minis): identify the board (the
//!        flashing scenario for it is in development).
//!   1. fetch the Capsule-14 OTA, extract the loader into ota/ (cached;
//!      one loader for all supported Rockchip boards)
//!   2. start our usbplug in RAM
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
use jb_core::target::{self, Target};
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
    about = "VK Capsula userspace jailbreak: dump -> adbd overlay in userdata -> ADB (Rockchip); board identification and u-boot command passthrough (Amlogic minis)"
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
    /// Live log of the download-mode USB events (2207:* and 1b8e:c003)
    Monitor,
    /// Amlogic mini: identify the board / run a u-boot burning command
    Aml {
        /// u-boot burning command to execute, e.g. "printenv bootdelay";
        /// omit to identify the board only
        cmd: Option<String>,
    },
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
        Some(Command::Aml { cmd }) => run_amlogic(wait, cmd.as_deref()),
    }
}

fn print_step(ev: Progress) {
    if let Progress::Step(msg) = ev {
        println!("{msg}");
    }
}

/// Step 0: prompt to connect the speaker and wait for a supported
/// download mode; the SoC vendor decides the scenario.
fn detect_target(wait: Option<Duration>) -> anyhow::Result<Target> {
    let config = Config::embedded();
    let mut hint = config.hint.text.clone();
    if !config.hint.amlogic_text.is_empty() {
        hint.push('\n');
        hint.push_str(&config.hint.amlogic_text);
    }
    target::wait_target(wait, &hint)
        .context("failed while waiting for a supported speaker on the bus")
}

/// Rockchip steps 1-3: loader, device bring-up, full dump into backup/.
/// Returns the live usbplug session (usable for the subsequent write).
fn rockchip_dump_all(
    wait: Option<Duration>,
) -> anyhow::Result<rockusb::LoaderSession<NusbTransport>> {
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
    match detect_target(wait)? {
        Target::Rockchip(_) => {
            let _session = rockchip_dump_all(wait)?;
            println!("done: dumps are in backup/ (nothing was written to the device)");
        }
        Target::Amlogic => {
            amlogic_scenario(wait)?;
            bail!("partition backup for the Amlogic mini is not implemented yet");
        }
    }
    Ok(())
}

/// Full jailbreak scenario (steps 0-6).
fn run_jailbreak(wait: Option<Duration>, yes: bool, format_userdata: bool) -> anyhow::Result<()> {
    match detect_target(wait)? {
        Target::Rockchip(_) => rockchip_jailbreak(wait, yes, format_userdata),
        Target::Amlogic => amlogic_scenario(wait),
    }
}

/// The Rockchip userspace-overwrite scenario (steps 1-6).
fn rockchip_jailbreak(
    wait: Option<Duration>,
    yes: bool,
    format_userdata: bool,
) -> anyhow::Result<()> {
    let mut s = rockchip_dump_all(wait)?;

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

/// The Amlogic path (late minis): bring-up + identify. The flashing
/// scenario for these boards is in development; the u-boot burning
/// command passthrough (`capsula-jb aml "<cmd>"`) is available.
fn amlogic_scenario(wait: Option<Duration>) -> anyhow::Result<()> {
    println!(
        "Amlogic mini detected. Power it off and on while connected if the \
         download window was missed."
    );
    let p = progress::CliProgress::new();
    let progress = |ev: Progress| p.handle(ev);
    let (_s, soc) = target::bring_up_amlogic(wait, &progress)?;
    println!("board: Amlogic download mode, {soc}");
    match soc.stage() {
        amlogic::Stage::Tpl => {
            println!("u-boot burning gadget is answering (the vendor factory window).");
        }
        amlogic::Stage::Ipl => {
            println!("BootROM download mode (no password required).");
        }
        _ => {}
    }
    println!("The flashing scenario for the Amlogic mini is in development.");
    println!("u-boot burning commands are available: capsula-jb aml \"<command>\"");
    Ok(())
}

/// `capsula-jb aml`: identify the Amlogic board / pass a u-boot burning
/// command through the download mode.
fn run_amlogic(wait: Option<Duration>, cmd: Option<&str>) -> anyhow::Result<()> {
    if cmd.is_none() {
        println!(
            "Connect the Amlogic mini and power it on: the u-boot download \
             window is ~0.5 s at every boot; power-cycle to retry."
        );
    }
    let p = progress::CliProgress::new();
    let progress = |ev: Progress| p.handle(ev);
    let (mut s, soc) = target::bring_up_amlogic(wait, &progress)?;
    match cmd {
        None => println!("{soc}"),
        Some(c) => {
            // the burning-command dispatcher exists only in the u-boot
            // gadget; the BootROM does not answer AM_REQ_BULKCMD
            if soc.stage() != amlogic::Stage::Tpl {
                bail!(
                    "the device is in the {} stage — burning commands are only                      available from the u-boot gadget; power-cycle the speaker                      to catch its download window",
                    soc.stage().label()
                );
            }
            let status = s.bulk_cmd(c)?;
            println!("{status}");
        }
    }
    Ok(())
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

fn ask_yes() -> anyhow::Result<()> {
    use std::io::{IsTerminal, Write};
    // Non-interactive stdin (CI, LLM agent shells, redirected input):
    // never hang on a prompt nobody can answer — fail with an
    // actionable message instead. Nothing has been written at this
    // point, so an exit is safe.
    if !std::io::stdin().is_terminal() {
        bail!(
            "confirmation required before writing userdata, but stdin is not \
             an interactive console; nothing was written. Re-run with --yes \
             once the write has been approved."
        );
    }
    std::io::stdout().flush().ok();
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    let ans = ans.trim().to_lowercase();
    if ans != "y" && ans != "yes" {
        bail!("aborted by user");
    }
    Ok(())
}

/// Live log of the download-mode bus events, both vendors.
fn monitor() -> anyhow::Result<()> {
    println!("USB monitor (2207:* rockchip, 1b8e:c003 amlogic), Ctrl-C to stop.");
    let mut rm = rockusb::DeviceMonitor::new();
    let mut amlogic_on_bus = amlogic::present()?;
    rm.poll()?;
    if let Some(best) = rockusb::transport::DeviceMode::best(rm.modes()) {
        println!(
            "[{}] on the bus now: 2207:{:04x} ({})",
            ts(),
            best.pid(),
            best.label()
        );
    }
    if amlogic_on_bus {
        println!(
            "[{}] on the bus now: 1b8e:c003 (amlogic download mode)",
            ts()
        );
    }
    loop {
        for ev in rm.poll()? {
            match ev {
                rockusb::monitor::MonitorEvent::Appeared(mode) => println!(
                    "[{}] appeared 2207:{:04x} ({})",
                    ts(),
                    mode.pid(),
                    mode.label()
                ),
                rockusb::monitor::MonitorEvent::Disappeared(mode) => println!(
                    "[{}] gone     2207:{:04x} ({})",
                    ts(),
                    mode.pid(),
                    mode.label()
                ),
            }
        }
        let now = amlogic::present()?;
        if now != amlogic_on_bus {
            if now {
                println!("[{}] appeared 1b8e:c003 (amlogic download mode)", ts());
            } else {
                println!("[{}] gone     1b8e:c003 (amlogic download mode)", ts());
            }
            amlogic_on_bus = now;
        }
        std::thread::sleep(jb_core::target::POLL_INTERVAL);
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
