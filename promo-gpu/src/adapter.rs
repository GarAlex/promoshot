//! Which adapter a headless render runs on, and what to say when there is
//! none.
//!
//! A CPU-only Linux box draws through a software Vulkan driver, and those
//! come and go with the machine: a redeploy that dropped
//! `mesa-vulkan-drivers` left `promo` with nothing but "no GPU adapter
//! available" (a reel user, 2026-10-27). So the Linux release carries its
//! own lavapipe in a `lavapipe/` folder beside the binary — Mesa's driver,
//! its dependencies and, for a box with no Vulkan loader at all, the loader
//! — and the choice runs in two passes:
//!
//! 1. The system's drivers, as the loader finds them. A hardware GPU or
//!    lavapipe there is used as is.
//! 2. When that gave nothing, or only SwiftShader, the bundled lavapipe:
//!    the loader is pointed at its manifest alone (`VK_DRIVER_FILES`, and
//!    `VK_ICD_FILENAMES` for loaders older than 1.3.207) and asked again.
//!
//! SwiftShader is the last resort, with a warning: it rendered the
//! reporter's 44 s reel in about 32 minutes where lavapipe took 77 s on
//! two cores. With nothing at all, the error lists every adapter tried and
//! the one-line fix.
//!
//! `PROMO_LAVAPIPE=<dir>` points at a bundle elsewhere; `PROMO_LAVAPIPE=off`
//! never uses one.

/// What kind of device an adapter is, in the order a render prefers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdapterKind {
    /// Google's CPU Vulkan: correct, and 10–50x slower than lavapipe.
    SwiftShader,
    /// Another CPU device (WARP, …): no known penalty worth a warning.
    OtherCpu,
    /// Mesa's llvmpipe-backed Vulkan.
    Lavapipe,
    /// A real GPU, integrated, discrete or virtual.
    Hardware,
}

/// Classifies an adapter from what it reports about itself.
pub fn kind_of(info: &wgpu::AdapterInfo) -> AdapterKind {
    let name = info.name.to_ascii_lowercase();
    let driver = info.driver.to_ascii_lowercase();
    // SwiftShader reports vendor 0x1AE0 (Google) and names itself.
    if name.contains("swiftshader") || driver.contains("swiftshader") || info.vendor == 0x1AE0 {
        return AdapterKind::SwiftShader;
    }
    if name.contains("llvmpipe") || driver.contains("llvmpipe") || name.contains("lavapipe") {
        return AdapterKind::Lavapipe;
    }
    match info.device_type {
        wgpu::DeviceType::Cpu => AdapterKind::OtherCpu,
        // `Other` is what some software drivers say; treat it as a CPU.
        wgpu::DeviceType::Other => AdapterKind::OtherCpu,
        _ => AdapterKind::Hardware,
    }
}

/// The best of a list by kind; the first wins a tie, so the loader's own
/// order breaks it.
pub fn best_index(kinds: &[AdapterKind]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, kind) in kinds.iter().enumerate() {
        if best.is_none_or(|b| *kind > kinds[b]) {
            best = Some(i);
        }
    }
    best
}

/// One pass at finding adapters, as the error reports it.
#[derive(Debug, Clone)]
pub struct Attempt {
    /// "system Vulkan drivers", "bundled lavapipe (…)".
    pub source: String,
    /// What the source said: the adapters it gave, or why there were none.
    pub outcome: String,
}

/// The whole "no adapter" message: every attempt, then the fix in one line.
pub fn no_adapter_message(attempts: &[Attempt], fix: &str) -> String {
    let mut text = String::from("no GPU adapter available. Tried:");
    for attempt in attempts {
        text.push_str(&format!("\n  - {}: {}", attempt.source, attempt.outcome));
    }
    text.push_str(&format!("\nFix: {fix}"));
    text
}

/// The one-line fix for a machine with no adapter.
pub fn fix_line() -> &'static str {
    if cfg!(target_os = "linux") {
        "sudo apt-get install -y mesa-vulkan-drivers libvulkan1   \
         (or keep the release's lavapipe/ folder beside promo, or set PROMO_LAVAPIPE=<that folder>)"
    } else if cfg!(target_os = "windows") {
        "update the graphics driver (any Direct3D 12 or Vulkan device renders)"
    } else {
        "this Mac has no Metal device available to this process"
    }
}

/// The once-per-process warning a SwiftShader render prints.
pub fn swiftshader_warning(name: &str) -> String {
    format!(
        "promo: warning: rendering on SwiftShader ({name}), a CPU Vulkan about 10-50x slower \
         than lavapipe. Install mesa-vulkan-drivers, or keep the release's lavapipe/ folder \
         beside promo."
    )
}

fn describe(adapters: &[(wgpu::AdapterInfo, AdapterKind)]) -> String {
    if adapters.is_empty() {
        return "no adapters".into();
    }
    adapters
        .iter()
        .map(|(info, kind)| format!("{} ({:?}, {:?})", info.name, info.backend, kind))
        .collect::<Vec<_>>()
        .join(", ")
}

fn make_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    })
}

/// Every adapter an instance offers, with its kind.
fn enumerate(instance: &wgpu::Instance) -> Vec<(wgpu::Adapter, wgpu::AdapterInfo, AdapterKind)> {
    instance
        .enumerate_adapters(wgpu::Backends::PRIMARY)
        .into_iter()
        .map(|adapter| {
            let info = adapter.get_info();
            let kind = kind_of(&info);
            (adapter, info, kind)
        })
        .collect()
}

/// The adapter a render runs on, its instance, and whether it is the
/// SwiftShader last resort (the caller warns); or the full error.
pub struct Chosen {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub kind: AdapterKind,
    /// The bundle's folder, when the adapter is the release's own lavapipe.
    pub bundled: Option<std::path::PathBuf>,
}

pub fn choose() -> Result<Chosen, String> {
    let mut attempts = Vec::new();

    // Pass 1: whatever the system's loader finds.
    let instance = make_instance();
    let mut found = enumerate(&instance);
    let kinds: Vec<AdapterKind> = found.iter().map(|f| f.2).collect();
    let infos: Vec<(wgpu::AdapterInfo, AdapterKind)> =
        found.iter().map(|f| (f.1.clone(), f.2)).collect();
    attempts.push(Attempt {
        source: system_source(),
        outcome: describe(&infos),
    });
    let best = best_index(&kinds);
    if let Some(i) = best {
        if found[i].2 > AdapterKind::SwiftShader {
            let (adapter, _, kind) = found.swap_remove(i);
            return Ok(Chosen {
                instance,
                adapter,
                kind,
                bundled: None,
            });
        }
    }

    // Pass 2: the lavapipe this release carries.
    #[cfg(target_os = "linux")]
    {
        match bundled::activate() {
            Ok(dir) => {
                let bundled_instance = make_instance();
                let mut more = enumerate(&bundled_instance);
                let infos: Vec<(wgpu::AdapterInfo, AdapterKind)> =
                    more.iter().map(|f| (f.1.clone(), f.2)).collect();
                attempts.push(Attempt {
                    source: format!("bundled lavapipe ({})", dir.display()),
                    outcome: describe(&infos),
                });
                let kinds: Vec<AdapterKind> = more.iter().map(|f| f.2).collect();
                if let Some(i) = best_index(&kinds) {
                    if more[i].2 > AdapterKind::SwiftShader {
                        let (adapter, _, kind) = more.swap_remove(i);
                        return Ok(Chosen {
                            instance: bundled_instance,
                            adapter,
                            kind,
                            bundled: Some(dir),
                        });
                    }
                }
            }
            Err(why) => attempts.push(Attempt {
                source: "bundled lavapipe".into(),
                outcome: why,
            }),
        }
    }

    // Last resort: the system's SwiftShader, which pass 1 kept.
    if let Some(i) = best {
        let (adapter, _, kind) = found.swap_remove(i);
        return Ok(Chosen {
            instance,
            adapter,
            kind,
            bundled: None,
        });
    }
    Err(no_adapter_message(&attempts, fix_line()))
}

#[cfg(not(target_os = "linux"))]
fn system_source() -> String {
    "system drivers".into()
}

#[cfg(target_os = "linux")]
fn system_source() -> String {
    let loader = if bundled::system_loader_present() {
        "Vulkan loader libvulkan.so.1 found"
    } else {
        "no Vulkan loader (libvulkan.so.1)"
    };
    let mut source = format!("system Vulkan drivers ({loader}");
    for key in ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"] {
        if let Ok(value) = std::env::var(key) {
            source.push_str(&format!(", {key}={value}"));
        }
    }
    source.push(')');
    source
}

#[cfg(target_os = "linux")]
pub mod bundled {
    //! The release's own lavapipe: `lavapipe/` beside the executable.
    use std::ffi::CString;
    use std::path::{Path, PathBuf};

    /// The manifest the bundle's loader is pointed at.
    pub const MANIFEST: &str = "lvp_icd.json";

    /// Where the bundle is, by `PROMO_LAVAPIPE` or beside the executable
    /// (through symlinks, so `~/.local/bin/promo` → the unpacked release
    /// still finds it); Err says why there is none.
    pub fn locate() -> Result<PathBuf, String> {
        if let Ok(value) = std::env::var("PROMO_LAVAPIPE") {
            if value.eq_ignore_ascii_case("off") || value == "0" {
                return Err("turned off (PROMO_LAVAPIPE=off)".into());
            }
            let dir = PathBuf::from(&value);
            return if dir.join(MANIFEST).is_file() {
                Ok(dir)
            } else {
                Err(format!("PROMO_LAVAPIPE={value} has no {MANIFEST}"))
            };
        }
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("cannot find this executable: {e}"))?;
        let dir = exe
            .parent()
            .map(|p| p.join("lavapipe"))
            .ok_or("the executable has no folder")?;
        if dir.join(MANIFEST).is_file() {
            Ok(dir)
        } else {
            Err(format!(
                "not present ({} missing)",
                dir.join(MANIFEST).display()
            ))
        }
    }

    fn dlopen(name: &Path, flags: i32) -> bool {
        let Ok(c) = CString::new(name.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        // SAFETY: a plain dlopen of a shared object by name; the handle is
        // kept for the process (never dlclosed), as the loader must stay.
        unsafe { !libc::dlopen(c.as_ptr(), flags).is_null() }
    }

    /// Whether the system has a Vulkan loader wgpu can open.
    pub fn system_loader_present() -> bool {
        dlopen(
            Path::new("libvulkan.so.1"),
            libc::RTLD_LAZY | libc::RTLD_LOCAL,
        )
    }

    /// Points the loader at the bundle: loads the bundle's own loader first
    /// when the system has none (wgpu then opens `libvulkan.so.1` by soname
    /// and gets this one), and sets the driver list to the bundle's
    /// manifest alone. Answers the bundle's folder.
    pub fn activate() -> Result<PathBuf, String> {
        let dir = locate()?;
        if !system_loader_present() {
            let loader = dir.join("libvulkan.so.1");
            if !loader.is_file() {
                return Err(format!(
                    "no Vulkan loader on the system, and none in {}",
                    dir.display()
                ));
            }
            if !dlopen(&loader, libc::RTLD_NOW | libc::RTLD_GLOBAL) {
                return Err(format!("{} would not load", loader.display()));
            }
        }
        let manifest = dir.join(MANIFEST);
        // Set before any thread of ours reads the environment: this runs
        // inside the one-time GPU bring-up, before a device exists.
        std::env::set_var("VK_DRIVER_FILES", &manifest);
        std::env::set_var("VK_ICD_FILENAMES", &manifest);
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        name: &str,
        driver: &str,
        vendor: u32,
        device_type: wgpu::DeviceType,
    ) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: name.into(),
            vendor,
            device: 0,
            device_type,
            driver: driver.into(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
        }
    }

    #[test]
    fn software_drivers_are_told_apart() {
        let lvp = info(
            "llvmpipe (LLVM 20.1.2, 256 bits)",
            "llvmpipe",
            0x10005,
            wgpu::DeviceType::Cpu,
        );
        let ss = info(
            "SwiftShader Device (Subzero)",
            "SwiftShader driver",
            0x1AE0,
            wgpu::DeviceType::Cpu,
        );
        let gpu = info("AMD Radeon", "radv", 0x1002, wgpu::DeviceType::DiscreteGpu);
        let warp = info(
            "Microsoft Basic Render Driver",
            "",
            0x1414,
            wgpu::DeviceType::Cpu,
        );
        assert_eq!(kind_of(&lvp), AdapterKind::Lavapipe);
        assert_eq!(kind_of(&ss), AdapterKind::SwiftShader);
        assert_eq!(kind_of(&gpu), AdapterKind::Hardware);
        assert_eq!(kind_of(&warp), AdapterKind::OtherCpu);
    }

    #[test]
    fn lavapipe_beats_swiftshader_and_a_gpu_beats_both() {
        use AdapterKind::*;
        assert_eq!(best_index(&[SwiftShader, Lavapipe]), Some(1));
        assert_eq!(best_index(&[Lavapipe, SwiftShader]), Some(0));
        assert_eq!(best_index(&[SwiftShader, Lavapipe, Hardware]), Some(2));
        assert_eq!(best_index(&[Lavapipe, Lavapipe]), Some(0));
        assert_eq!(best_index(&[]), None);
    }

    #[test]
    fn the_error_lists_every_attempt_and_ends_on_the_fix() {
        let text = no_adapter_message(
            &[
                Attempt {
                    source: "system Vulkan drivers (no Vulkan loader (libvulkan.so.1))".into(),
                    outcome: "no adapters".into(),
                },
                Attempt {
                    source: "bundled lavapipe".into(),
                    outcome: "not present (/opt/promo/lavapipe/lvp_icd.json missing)".into(),
                },
            ],
            "sudo apt-get install -y mesa-vulkan-drivers libvulkan1",
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "no GPU adapter available. Tried:");
        assert!(lines[1].contains("system Vulkan drivers") && lines[1].ends_with("no adapters"));
        assert!(lines[2].contains("bundled lavapipe") && lines[2].contains("missing"));
        assert_eq!(
            lines.last().unwrap(),
            &"Fix: sudo apt-get install -y mesa-vulkan-drivers libvulkan1"
        );
        assert_eq!(lines.len(), 4);
    }
}
