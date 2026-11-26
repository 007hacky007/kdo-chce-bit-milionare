//! Embeds `packaging/windows/app.manifest` (per-monitor DPI v2, UTF-8 code page) into the
//! Windows executable.
//!
//! - MSVC targets: through the linker (`/MANIFEST:EMBED /MANIFESTINPUT:`), no resource
//!   compiler needed.
//! - GNU targets (llvm-mingw, `x86_64-pc-windows-gnullvm`): `packaging/windows/app.rc` is
//!   compiled with windres (`RC_<target>`, `RC`, or `x86_64-w64-mingw32-windres`) into a
//!   COFF object that is linked into the binaries. It also carries the version resource,
//!   rendered from `packaging/windows/version.rc.in` with the package version, and the
//!   icon when `MILIONAR_WINDOWS_ICON` names an `.ico` (set by
//!   `packaging/windows/build-tree.sh`, which draws it at build time).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../packaging/windows");
    // A Windows host would get a verbatim \\?\ path, which the resource
    // compiler may not take; the relative path is fine there.
    let dir = if cfg!(windows) {
        dir
    } else {
        dir.canonicalize().unwrap_or(dir)
    };
    let manifest = dir.join("app.manifest");
    let rc = dir.join("app.rc");
    let version_in = dir.join("version.rc.in");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", rc.display());
    println!("cargo:rerun-if-changed={}", version_in.display());
    println!("cargo:rerun-if-env-changed=MILIONAR_WINDOWS_ICON");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" {
        return;
    }
    if env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
        return;
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    let windres = [format!("RC_{}", target.replace('-', "_")), "RC".to_string()]
        .iter()
        .find_map(|k| {
            println!("cargo:rerun-if-env-changed={k}");
            std::env::var(k).ok()
        })
        .unwrap_or_else(|| "x86_64-w64-mingw32-windres".to_string());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut text = render_version(
        &std::fs::read_to_string(&version_in).expect("read version.rc.in"),
        &version,
    );
    if let Some(icon) = std::env::var_os("MILIONAR_WINDOWS_ICON").filter(|v| !v.is_empty()) {
        let icon = PathBuf::from(icon);
        println!("cargo:rerun-if-changed={}", icon.display());
        let path = icon.to_str().expect("MILIONAR_WINDOWS_ICON is not UTF-8");
        assert!(
            icon.is_file() && !path.contains('"'),
            "MILIONAR_WINDOWS_ICON={path} is not a usable .ico file"
        );
        // The first icon group is what Explorer shows and SDL uses for the window.
        text.push_str(&format!("\n1 ICON \"{}\"\n", path.replace('\\', "/")));
    }
    std::fs::write(out_dir.join("version.rc"), text).expect("write version.rc");
    let out = out_dir.join("app-manifest.o");
    let status = Command::new(&windres)
        .current_dir(&dir)
        .arg("-I")
        .arg(&out_dir)
        .args(["-O", "coff", "-i"])
        .arg(&rc)
        .arg("-o")
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        other => panic!("cannot run {windres} for the Windows manifest: {other:?}"),
    }
}

/// `version.rc.in` with `@VERSION@` and `@VERSION_COMMA@` (`a,b,c,0`) filled in.
fn render_version(template: &str, version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut nums: Vec<u16> = core
        .split('.')
        .map(|n| n.parse().expect("numeric package version"))
        .collect();
    assert!(
        nums.len() <= 4,
        "package version {version} has more than 4 parts"
    );
    nums.resize(4, 0);
    let comma = nums
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    template
        .replace("@VERSION_COMMA@", &comma)
        .replace("@VERSION@", version)
}
