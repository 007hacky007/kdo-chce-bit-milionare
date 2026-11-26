//! The pinned Real-ESRGAN runner as a subprocess, and a fake for tests.
//!
//! The runner binary and models are pinned by SHA-256 in
//! `tools/upscalers/upscaler.lock.json` (fetched by
//! `tools/fetch-upscaler.sh`). Arguments are fixed: model
//! `realesr-animevideov3`, scale 3, tile 128, no TTA, gpu 0, PNG output.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::pack::{PackError, RunnerParams};

pub const MODEL_NAME: &str = "realesr-animevideov3";
pub const SCALE: u32 = 3;
pub const TILE: u32 = 128;
pub const GPU: i32 = 0;

/// Runner binary name inside the runner directory.
#[cfg(windows)]
pub const RUNNER_EXE: &str = "realesrgan-ncnn-vulkan.exe";
#[cfg(not(windows))]
pub const RUNNER_EXE: &str = "realesrgan-ncnn-vulkan";

const RUNNER_SHA256_MACOS: &str =
    "c1c35d92079085de96b9d547fd7e4464bc8a2e9ccf28d7b8c712d72ade91b7cc";
const RUNNER_SHA256_WINDOWS: &str =
    "07e49f7cbb4ede01ae4dd4c399d3a7e5846e3d2085c3128eff881e55cb7b1a0c";

/// Pinned runner SHA-256 for this platform (lock file `runners`), if any.
pub const RUNNER_SHA256: Option<&str> = if cfg!(target_os = "macos") {
    Some(RUNNER_SHA256_MACOS)
} else if cfg!(windows) {
    Some(RUNNER_SHA256_WINDOWS)
} else {
    None
};

/// Every pinned runner build (one per platform, same release and models).
pub const PINNED_RUNNER_SHA256S: [&str; 2] = [RUNNER_SHA256_MACOS, RUNNER_SHA256_WINDOWS];

/// True when `p` records a pinned runner build (any platform), the pinned model and the
/// fixed parameters, i.e. a pack made by `milionar-tool upscale` with the real runner
/// (not the test stand-in).
pub fn is_pinned(p: &RunnerParams) -> bool {
    PINNED_RUNNER_SHA256S.contains(&hex(&p.runner_sha256).as_str())
        && hex(&p.model_param_sha256) == MODEL_PARAM_SHA256
        && hex(&p.model_bin_sha256) == MODEL_BIN_SHA256
        && p.model_name == MODEL_NAME
        && p.scale == SCALE
        && p.tile == TILE
        && !p.tta
        && p.gpu == GPU
}
pub const MODEL_PARAM_FILE: &str = "realesr-animevideov3-x3.param";
pub const MODEL_PARAM_SHA256: &str =
    "d1a5755008791d09b57e3425fc9dd0bd26b00fdf79c606210bc0e693f8230881";
pub const MODEL_BIN_FILE: &str = "realesr-animevideov3-x3.bin";
pub const MODEL_BIN_SHA256: &str =
    "548a36f9c3f4ab8da56cd3b13badf23968bee207b396dad14d04b830e5f2ab2d";

/// Keep at most this much of the runner's stderr for error messages.
const STDERR_KEEP: usize = 8 * 1024;
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum UpscaleError {
    #[error("upscale i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error("cannot read game data: {0}")]
    Game(String),
    #[error("no pinned upscaler runner for this platform")]
    UnsupportedPlatform,
    #[error("upscaler file {} is missing; run tools/fetch-upscaler.sh", .0.display())]
    RunnerMissing(PathBuf),
    #[error("upscaler file {} has sha256 {found}, expected {expected}; run tools/fetch-upscaler.sh", path.display())]
    HashMismatch {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("cannot start the upscaler {}: {source}", path.display())]
    Spawn {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the upscaler found no usable Vulkan GPU (it needs Vulkan, or Metal through MoltenVK on macOS); the game still runs with the original art. Runner output: {stderr}"
    )]
    NoVulkan { stderr: String },
    #[error(
        "the upscaler crashed ({how}); the game still runs with the original art. Runner output: {stderr}"
    )]
    RunnerCrashed { how: String, stderr: String },
    #[error("the upscaler failed with exit code {code}. Runner output: {stderr}")]
    RunnerFailed { code: i32, stderr: String },
    #[error("the upscaler wrote no output for {file}. Runner output: {stderr}")]
    MissingOutput { file: String, stderr: String },
    #[error("bad upscaler output {file}: {reason}")]
    BadOutput { file: String, reason: String },
    #[error("upscale cancelled")]
    Cancelled,
    #[error("another upscale is running (lock {} is held)", .0.display())]
    Busy(PathBuf),
    #[error("not enough free disk space for upscaling: {needed} bytes needed, {available} free")]
    NoSpace { needed: u64, available: u64 },
}

/// Free space the upscale step asks for before it starts: the resume cache,
/// the staging pack and an old pack side by side (about 1.1 GB each), plus
/// a margin.
pub const FREE_SPACE_NEEDED: u64 = 3584 << 20;

/// [`UpscaleError::NoSpace`] unless the disk holding `dir` has `needed`
/// bytes free.
pub fn check_space(dir: &Path, needed: u64) -> Result<()> {
    let available = fs4::available_space(dir)?;
    if available < needed {
        return Err(UpscaleError::NoSpace { needed, available });
    }
    Ok(())
}

/// Runner directory name in the repository (as `tools/fetch-upscaler.sh`
/// extracts it) and in a copied tool tree.
#[cfg(windows)]
pub const RUNNER_DIR_NAME: &str = "realesrgan-20220424-windows";
#[cfg(not(windows))]
pub const RUNNER_DIR_NAME: &str = "realesrgan-20220424";

/// Where the runner is looked for, relative to the directory of the running
/// executable (never the working directory), first match wins:
/// - `upscaler/` next to the executable (the Windows install tree);
/// - `../Resources/upscaler/` (a macOS app: `Contents/MacOS/<exe>`);
/// - `upscalers/<name>` next to the executable (a copied tool);
/// - `../../tools/upscalers/<name>` (a repository build in `target/<profile>/`);
/// - `../../../tools/upscalers/<name>` (a cross build in `target/<triple>/<profile>/`),
///   never above a directory holding `.git`.
pub fn runner_dir_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![exe_dir.join("upscaler")];
    if let Some(contents) = exe_dir.parent() {
        out.push(contents.join("Resources").join("upscaler"));
    }
    out.push(exe_dir.join("upscalers").join(RUNNER_DIR_NAME));
    let rel = Path::new("tools").join("upscalers").join(RUNNER_DIR_NAME);
    let mut up = exe_dir.to_path_buf();
    for depth in 1..=3 {
        if !up.pop() {
            break;
        }
        // `<exe>/../tools` is never a repository layout; start at two levels up.
        if depth >= 2 {
            out.push(up.join(&rel));
        }
        // Never look above the repository root.
        if up.join(".git").exists() {
            break;
        }
    }
    out
}

/// The first of [`runner_dir_candidates`] holding the runner executable, or
/// every place looked in.
pub fn find_runner_dir(exe_dir: &Path) -> std::result::Result<PathBuf, Vec<PathBuf>> {
    let c = runner_dir_candidates(exe_dir);
    match c.iter().find(|d| d.join(RUNNER_EXE).is_file()) {
        Some(d) => Ok(d.clone()),
        None => Err(c),
    }
}

/// What the runner said about the Vulkan device it uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuProbe {
    /// Name of device 0 (the one the runner is told to use), if printed.
    pub device: Option<String>,
    /// The device is a software rasteriser on the CPU (llvmpipe/lavapipe,
    /// SwiftShader): upscaling would take hours.
    pub cpu_only: bool,
}

/// Device names that mean Vulkan runs on the CPU.
const CPU_DEVICES: [&str; 3] = ["llvmpipe", "lavapipe", "swiftshader"];

/// Reads the device line ncnn prints at start-up (`[0 Apple M1 Pro]  queueC=...`).
pub fn probe_from_stderr(stderr: &str) -> GpuProbe {
    let device = stderr.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix("[0 ")?;
        let end = rest.find("]  ").or_else(|| rest.rfind(']'))?;
        Some(rest[..end].trim().to_string()).filter(|d| !d.is_empty())
    });
    let cpu_only = device.as_ref().is_some_and(|d| {
        let low = d.to_ascii_lowercase();
        CPU_DEVICES.iter().any(|c| low.contains(c))
    });
    GpuProbe { device, cpu_only }
}

pub type Result<T> = std::result::Result<T, UpscaleError>;

/// Something that upscales every `*.png` in a directory by [`SCALE`] into
/// another directory under the same names (RGB or RGBA PNG out).
pub trait Upscaler {
    /// Facts recorded in the pack header and the cache stamp.
    fn params(&self) -> RunnerParams;
    /// Upscales `in_dir/*.png` into `out_dir`. Returns
    /// [`UpscaleError::Cancelled`] soon after `cancel` is set.
    fn upscale_dir(&self, in_dir: &Path, out_dir: &Path, cancel: &AtomicBool) -> Result<()>;

    /// Upscales one tiny image to learn which device does the work (and to
    /// fail early without Vulkan). The default knows nothing about a device.
    fn probe(&self, _cancel: &AtomicBool) -> Result<GpuProbe> {
        Ok(GpuProbe::default())
    }
}

/// The real runner, located and verified.
#[derive(Debug, Clone)]
pub struct Runner {
    exe: PathBuf,
    models: PathBuf,
    params: RunnerParams,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpscaleError::RunnerMissing(path.to_path_buf()));
        }
        Err(e) => return Err(e.into()),
    };
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

fn verify(path: &Path, expected: &str) -> Result<[u8; 32]> {
    let got = sha256_file(path)?;
    if hex(&got) != expected {
        return Err(UpscaleError::HashMismatch {
            path: path.to_path_buf(),
            expected: expected.into(),
            found: hex(&got),
        });
    }
    Ok(got)
}

/// `s` without a Windows verbatim prefix when that is safe: `\\?\C:\dir\file`
/// becomes `C:\dir\file`. Anything else (no prefix, `\\?\UNC\...`, a
/// volume GUID, a path of [`MAX_PLAIN_PATH`] or more characters, a component
/// that Win32 would change without the prefix: a trailing dot or space, a
/// reserved DOS device name, a `/`) is `None`, and the caller keeps `s`.
///
/// `std::fs::canonicalize` returns verbatim paths on Windows. The runner
/// builds model paths as `<-m dir>/<name>`, and Win32 does not turn `/` into
/// `\` under the verbatim prefix, so the runner must get a plain path. Pure
/// string work, the same on every OS.
pub fn strip_verbatim(s: &str) -> Option<&str> {
    let rest = s.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return None;
    }
    if rest.chars().count() >= MAX_PLAIN_PATH || rest.contains('/') {
        return None;
    }
    let plain = rest[3..].split('\\').all(|c| {
        if c.is_empty() {
            // Only a trailing separator ("C:\\" or "C:\dir\\") is fine.
            return true;
        }
        if c == "." || c == ".." || c.ends_with('.') || c.ends_with(' ') {
            return false;
        }
        let stem = c
            .split('.')
            .next()
            .unwrap_or(c)
            .trim_end()
            .to_ascii_lowercase();
        !RESERVED_NAMES.contains(&stem.as_str())
            && !((stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.chars().count() == 4
                && !stem[3..].starts_with('0')
                && stem[3..]
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || matches!(ch, '\u{b9}' | '\u{b2}' | '\u{b3}')))
    });
    let no_doubles = !rest[3..].contains(r"\\");
    (plain && no_doubles).then_some(rest)
}

/// Paths this long or longer keep their verbatim prefix (`MAX_PATH`).
pub const MAX_PLAIN_PATH: usize = 260;

const RESERVED_NAMES: [&str; 6] = ["con", "prn", "aux", "nul", "conin$", "conout$"];

/// `p` as a path Win32 takes without the verbatim prefix (see
/// [`strip_verbatim`]). Only Windows changes anything: elsewhere `\\?\` is
/// an ordinary file name.
pub fn plain_path(p: &Path) -> PathBuf {
    if cfg!(windows)
        && let Some(s) = p.to_str().and_then(strip_verbatim)
    {
        return PathBuf::from(s);
    }
    p.to_path_buf()
}

fn params(runner: [u8; 32], param: [u8; 32], bin: [u8; 32]) -> RunnerParams {
    RunnerParams {
        runner_sha256: runner,
        model_param_sha256: param,
        model_bin_sha256: bin,
        model_name: MODEL_NAME.into(),
        scale: SCALE,
        tile: TILE,
        tta: false,
        gpu: GPU,
    }
}

impl Runner {
    /// Finds `dir/<RUNNER_EXE>` and `dir/models/realesr-animevideov3-x3.*`
    /// and checks their SHA-256 against the pins.
    pub fn locate(dir: &Path) -> Result<Runner> {
        let pin = RUNNER_SHA256.ok_or(UpscaleError::UnsupportedPlatform)?;
        let exe = dir.join(RUNNER_EXE);
        let models = dir.join("models");
        let r = verify(&exe, pin)?;
        let p = verify(&models.join(MODEL_PARAM_FILE), MODEL_PARAM_SHA256)?;
        let b = verify(&models.join(MODEL_BIN_FILE), MODEL_BIN_SHA256)?;
        Ok(Runner {
            exe,
            models,
            params: params(r, p, b),
        })
    }

    /// A runner without hash checks (tests drive the subprocess handling
    /// with scripts). The params record all-zero hashes.
    #[doc(hidden)]
    pub fn unverified(exe: &Path, models: &Path) -> Runner {
        Runner {
            exe: exe.to_path_buf(),
            models: models.to_path_buf(),
            params: params([0; 32], [0; 32], [0; 32]),
        }
    }

    fn args(&self, in_dir: &Path, out_dir: &Path) -> Vec<std::ffi::OsString> {
        let mut a: Vec<std::ffi::OsString> = vec![
            "-i".into(),
            plain_path(in_dir).into(),
            "-o".into(),
            plain_path(out_dir).into(),
        ];
        for s in [
            "-n",
            MODEL_NAME,
            "-s",
            &SCALE.to_string(),
            "-t",
            &TILE.to_string(),
            "-g",
            &GPU.to_string(),
            "-f",
            "png",
            "-m",
        ] {
            a.push(s.into());
        }
        a.push(plain_path(&self.models).into());
        a
    }

    /// The runner command: fixed arguments, no stdin or stdout, stderr
    /// piped, and the platform setup of [`proc::configure`].
    fn command(&self, in_dir: &Path, out_dir: &Path) -> Command {
        let mut c = Command::new(plain_path(&self.exe));
        c.args(self.args(in_dir, out_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        proc::configure(&mut c);
        c
    }
}

/// `CREATE_NO_WINDOW`: the console runner gets no console window when the
/// tool or the game runs it on Windows.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
const _: () = assert!(CREATE_NO_WINDOW == windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);

/// Ties the runner process to this one.
///
/// - Unix: the runner gets its own process group (`process_group(0)`), and
///   cancelling kills the whole group, so helpers it forks die too.
/// - Windows: the runner is started with [`CREATE_NO_WINDOW`] and assigned
///   to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The job
///   handle is only held by this process, so the runner dies with the app
///   even when the app is killed. The runner runs for a moment before it
///   is assigned (std cannot start it suspended); it starts no helpers.
#[cfg(unix)]
mod proc {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    pub fn configure(c: &mut Command) {
        c.process_group(0);
    }

    pub struct Guard {
        pgid: Option<rustix::process::Pid>,
    }

    impl Guard {
        pub fn attach(child: &Child) -> Guard {
            Guard {
                pgid: i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw),
            }
        }

        /// Kills the runner's process group. Called before the runner is
        /// reaped, so its pid (the group id) cannot have been reused.
        pub fn kill(&self, child: &mut Child) {
            if let Some(g) = self.pgid {
                let _ = rustix::process::kill_process_group(g, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
mod proc {
    use std::os::windows::io::OwnedHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    pub fn configure(c: &mut Command) {
        c.creation_flags(super::CREATE_NO_WINDOW);
    }

    pub struct Guard {
        /// `None` when the job could not be created or assigned (the runner
        /// then still runs; cancel kills it directly).
        job: Option<OwnedHandle>,
    }

    impl Guard {
        pub fn attach(child: &Child) -> Guard {
            let job = super::job::kill_on_close()
                .and_then(|j| super::job::assign(&j, child).map(|()| j))
                .ok();
            Guard { job }
        }

        pub fn kill(&self, child: &mut Child) {
            if let Some(j) = &self.job {
                super::job::terminate(j);
            }
            let _ = child.kill();
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod proc {
    use std::process::{Child, Command};

    pub fn configure(_: &mut Command) {}

    pub struct Guard;

    impl Guard {
        pub fn attach(_: &Child) -> Guard {
            Guard
        }

        pub fn kill(&self, child: &mut Child) {
            let _ = child.kill();
        }
    }
}

/// Windows Job Object FFI. The only unsafe code in this crate: three
/// kernel32 calls on handles this module owns or borrows for the call.
#[cfg(windows)]
#[allow(unsafe_code)]
mod job {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// A new unnamed job whose processes are killed when its last handle
    /// closes.
    pub fn kill_on_close() -> std::io::Result<OwnedHandle> {
        // SAFETY: null security attributes and a null name are documented
        // as valid; the result is checked before use.
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `h` is a fresh, valid handle that nothing else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(h) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a live, correctly sized struct of the type the
        // information class names; the handle is valid for the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn assign(job: &OwnedHandle, child: &Child) -> std::io::Result<()> {
        // SAFETY: both handles are valid for the duration of the call; std
        // opens the child with full access (PROCESS_SET_QUOTA and
        // PROCESS_TERMINATE included).
        let ok = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn terminate(job: &OwnedHandle) {
        // SAFETY: the job handle is valid for the call.
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
    }
}

impl Upscaler for Runner {
    fn params(&self) -> RunnerParams {
        self.params.clone()
    }

    fn upscale_dir(&self, in_dir: &Path, out_dir: &Path, cancel: &AtomicBool) -> Result<()> {
        self.run(in_dir, out_dir, cancel).map(drop)
    }

    fn probe(&self, cancel: &AtomicBool) -> Result<GpuProbe> {
        let work = tempfile::Builder::new()
            .prefix("milionar-probe-")
            .tempdir()?;
        let (i, o) = (work.path().join("in"), work.path().join("out"));
        std::fs::create_dir_all(&i)?;
        crate::img::write_png_rgb(&i.join("probe.png"), 4, 4, &[128; 4 * 4 * 3])?;
        let stderr = self.run(&i, &o, cancel)?;
        Ok(probe_from_stderr(&stderr))
    }
}

impl Runner {
    /// Runs the runner over `in_dir`; its stderr on success.
    fn run(&self, in_dir: &Path, out_dir: &Path, cancel: &AtomicBool) -> Result<String> {
        std::fs::create_dir_all(out_dir)?;
        let mut child =
            self.command(in_dir, out_dir)
                .spawn()
                .map_err(|source| UpscaleError::Spawn {
                    path: self.exe.clone(),
                    source,
                })?;
        let guard = proc::Guard::attach(&child);
        let mut pipe = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut keep = Vec::new();
            if let Some(p) = pipe.as_mut() {
                let mut buf = [0u8; 4096];
                while let Ok(n) = p.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    keep.extend_from_slice(&buf[..n]);
                    if keep.len() > 2 * STDERR_KEEP {
                        keep.drain(..keep.len() - STDERR_KEEP);
                    }
                }
            }
            keep
        });
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if cancel.load(Ordering::Relaxed) {
                guard.kill(&mut child);
                let _ = child.wait();
                // Not joined: a grandchild may still hold the pipe open.
                drop(reader);
                return Err(UpscaleError::Cancelled);
            }
            std::thread::sleep(POLL);
        };
        let stderr = reader.join().unwrap_or_default();
        let start = stderr.len().saturating_sub(STDERR_KEEP);
        let stderr = String::from_utf8_lossy(&stderr[start..]).trim().to_string();
        classify(status, stderr.clone())?;
        // The runner exits 0 even when it cannot decode or encode an image.
        for name in png_names(in_dir)? {
            if !out_dir.join(&name).is_file() {
                return Err(UpscaleError::MissingOutput { file: name, stderr });
            }
        }
        Ok(stderr)
    }
}

/// Text the runner (ncnn) prints when there is no Vulkan instance or device.
const NO_VULKAN: [&str; 7] = [
    "invalid gpu device",
    "vkcreateinstance failed",
    "vkenumeratephysicaldevices failed",
    "no vulkan",
    "vulkan-1.dll",
    "vk_error_incompatible_driver",
    "vk_error_initialization_failed",
];

/// Maps the runner's exit to an error (`Ok` for a clean exit).
pub fn classify(status: ExitStatus, stderr: String) -> Result<()> {
    let low = stderr.to_ascii_lowercase();
    let no_vulkan = NO_VULKAN.iter().any(|p| low.contains(p));
    if status.success() {
        return Ok(());
    }
    if no_vulkan {
        return Err(UpscaleError::NoVulkan { stderr });
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Err(UpscaleError::RunnerCrashed {
                how: format!("signal {sig}"),
                stderr,
            });
        }
    }
    let code = status.code().unwrap_or(-1);
    // Windows NTSTATUS exits: STATUS_DLL_NOT_FOUND usually means no
    // vulkan-1.dll (no Vulkan driver); other 0xC0000000 codes are crashes.
    let nt = code as u32;
    if cfg!(windows) && nt == 0xC000_0135 {
        return Err(UpscaleError::NoVulkan {
            stderr: format!("a required DLL (usually vulkan-1.dll) is missing. {stderr}"),
        });
    }
    if cfg!(windows) && nt & 0xF000_0000 == 0xC000_0000 {
        return Err(UpscaleError::RunnerCrashed {
            how: format!("exception 0x{nt:08X}"),
            stderr,
        });
    }
    Err(UpscaleError::RunnerFailed { code, stderr })
}

/// Sorted names of the `*.png` files in `dir`.
pub(crate) fn png_names(dir: &Path) -> Result<Vec<String>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".png") && e.file_type()?.is_file() {
            v.push(name);
        }
    }
    v.sort();
    Ok(v)
}

/// Nearest-neighbour 3x in Rust, for tests and CI without a GPU. Counts
/// invocations and images so tests can check dedupe and resume.
#[derive(Debug, Default)]
pub struct FakeRunner {
    pub invocations: AtomicUsize,
    pub images: AtomicUsize,
}

impl FakeRunner {
    pub fn new() -> FakeRunner {
        FakeRunner::default()
    }

    /// The params a fake pack records (distinct from any real runner).
    pub fn fake_params() -> RunnerParams {
        RunnerParams {
            runner_sha256: [0xfa; 32],
            model_param_sha256: [0; 32],
            model_bin_sha256: [0; 32],
            model_name: "fake-nearest".into(),
            scale: SCALE,
            tile: TILE,
            tta: false,
            gpu: GPU,
        }
    }
}

impl Upscaler for FakeRunner {
    fn params(&self) -> RunnerParams {
        FakeRunner::fake_params()
    }

    fn upscale_dir(&self, in_dir: &Path, out_dir: &Path, cancel: &AtomicBool) -> Result<()> {
        self.invocations.fetch_add(1, Ordering::Relaxed);
        std::fs::create_dir_all(out_dir)?;
        for name in png_names(in_dir)? {
            if cancel.load(Ordering::Relaxed) {
                return Err(UpscaleError::Cancelled);
            }
            let (w, h, rgb) = crate::img::read_png_rgb(&in_dir.join(&name))?;
            let s = SCALE as usize;
            let (w, h) = (w as usize, h as usize);
            let mut out = Vec::with_capacity(w * h * s * s * 3);
            for y in 0..h * s {
                for x in 0..w * s {
                    let i = ((y / s) * w + x / s) * 3;
                    out.extend_from_slice(&rgb[i..i + 3]);
                }
            }
            crate::img::write_png_rgb(&out_dir.join(&name), (w * s) as u32, (h * s) as u32, &out)?;
            self.images.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_verbatim_drive_paths() {
        let ok = [
            (
                r"\\?\C:\Users\ada-example\AppData\Local\Programs\Milionar\upscaler",
                r"C:\Users\ada-example\AppData\Local\Programs\Milionar\upscaler",
            ),
            (r"\\?\d:\x", r"d:\x"),
            (r"\\?\C:\", r"C:\"),
            (
                "\\\\?\\C:\\Hry\\Kdo chce b\u{ed}t\\models",
                "C:\\Hry\\Kdo chce b\u{ed}t\\models",
            ),
            (
                r"\\?\C:\a.b\cons.d\com0\console",
                r"C:\a.b\cons.d\com0\console",
            ),
        ];
        for (inp, want) in ok {
            assert_eq!(strip_verbatim(inp), Some(want), "{inp}");
        }
    }

    #[test]
    fn strip_verbatim_keeps_what_win32_would_change() {
        let long = format!(r"\\?\C:\{}", "a".repeat(MAX_PLAIN_PATH));
        let keep = [
            r"C:\plain",
            "/Users/ada-example/x",
            "",
            r"\\?\",
            r"\\?\C:",
            r"\\?\C:x",
            r"\\?\1:\x",
            r"\\?\UNC\server\share\x",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"\\.\C:\x",
            r"\\?\C:\a/b",
            r"\\?\C:\a\\b",
            r"\\?\C:\a\.\b",
            r"\\?\C:\a\..\b",
            r"\\?\C:\dot.\b",
            r"\\?\C:\space \b",
            r"\\?\C:\x\NUL",
            r"\\?\C:\x\con.txt",
            r"\\?\C:\x\Com1",
            r"\\?\C:\x\lpt9.log",
            "\\\\?\\C:\\x\\com\u{b9}",
            r"\\?\C:\x\aux .txt",
            long.as_str(),
        ];
        for inp in keep {
            assert_eq!(strip_verbatim(inp), None, "{inp}");
        }
    }

    #[test]
    fn plain_path_changes_only_windows_verbatim_paths() {
        let p = Path::new(r"\\?\C:\r\models");
        let want = if cfg!(windows) {
            r"C:\r\models"
        } else {
            r"\\?\C:\r\models"
        };
        assert_eq!(plain_path(p), Path::new(want));
        assert_eq!(plain_path(Path::new("/r/models")), Path::new("/r/models"));
    }

    #[cfg(windows)]
    #[test]
    fn runner_args_have_no_verbatim_paths() {
        let r = Runner::unverified(
            Path::new(r"\\?\C:\r\runner.exe"),
            Path::new(r"\\?\C:\r\models"),
        );
        let c = r.command(Path::new(r"\\?\C:\w\in"), Path::new(r"\\?\C:\w\out"));
        assert_eq!(c.get_program(), r"C:\r\runner.exe");
        for a in c.get_args() {
            assert!(!a.to_string_lossy().starts_with(r"\\?\"), "{a:?}");
        }
    }

    #[test]
    fn command_has_fixed_arguments() {
        let r = Runner::unverified(Path::new("/r/runner"), Path::new("/r/models"));
        let c = r.command(Path::new("/w/in"), Path::new("/w/out"));
        assert_eq!(c.get_program(), "/r/runner");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-i",
                "/w/in",
                "-o",
                "/w/out",
                "-n",
                "realesr-animevideov3",
                "-s",
                "3",
                "-t",
                "128",
                "-g",
                "0",
                "-f",
                "png",
                "-m",
                "/r/models"
            ]
        );
    }

    #[test]
    fn gpu_named_in_stderr() {
        let p = probe_from_stderr(
            "[0 Apple M1 Pro]  queueC=0[1]  queueG=0[1]\n[0 Apple M1 Pro]  fp16-p/s/a=1/1/1\n0.00%",
        );
        assert_eq!(p.device.as_deref(), Some("Apple M1 Pro"));
        assert!(!p.cpu_only);
        for (line, name) in [
            (
                "[0 llvmpipe (LLVM 15.0.7, 256 bits)]  queueC=0[1]",
                "llvmpipe (LLVM 15.0.7, 256 bits)",
            ),
            (
                "[0 SwiftShader Device (Subzero)]  queueC=0[1]",
                "SwiftShader Device (Subzero)",
            ),
            ("[0 Lavapipe]  queueC=0[1]", "Lavapipe"),
        ] {
            let p = probe_from_stderr(line);
            assert!(p.cpu_only, "{line}");
            assert_eq!(p.device.as_deref(), Some(name));
        }
        // gpu 0 is what the runner uses; other devices do not count.
        let p = probe_from_stderr(
            "[0 NVIDIA GeForce RTX 3060]  queueC=2[8]\n[1 llvmpipe (LLVM 15)]  queueC=0[1]",
        );
        assert_eq!(p.device.as_deref(), Some("NVIDIA GeForce RTX 3060"));
        assert!(!p.cpu_only);
        assert_eq!(probe_from_stderr("0.00%"), GpuProbe::default());
    }

    #[test]
    fn runner_dirs_of_the_app_layouts() {
        let root = tempfile::tempdir().unwrap();
        // A repository build in target/<profile>/.
        let exe_dir = root.path().join("target").join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let dev = root.path().join("tools/upscalers").join(RUNNER_DIR_NAME);
        let c = runner_dir_candidates(&exe_dir);
        assert_eq!(
            c,
            vec![
                exe_dir.join("upscaler"),
                root.path().join("target/Resources/upscaler"),
                exe_dir.join("upscalers").join(RUNNER_DIR_NAME),
                dev.clone(),
            ]
        );
        let err = find_runner_dir(&exe_dir).unwrap_err();
        assert_eq!(err, c);
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(RUNNER_EXE), b"").unwrap();
        assert_eq!(find_runner_dir(&exe_dir).unwrap(), dev);
        // An installed tree (Windows: <install>/upscaler) wins.
        std::fs::create_dir_all(exe_dir.join("upscaler")).unwrap();
        std::fs::write(exe_dir.join("upscaler").join(RUNNER_EXE), b"").unwrap();
        assert_eq!(find_runner_dir(&exe_dir).unwrap(), exe_dir.join("upscaler"));
        // The macOS app bundle: Contents/MacOS/<exe> and Contents/Resources/upscaler.
        let app = root.path().join("Kdo chce bít milionáře.app/Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Resources/upscaler")).unwrap();
        std::fs::write(app.join("Resources/upscaler").join(RUNNER_EXE), b"").unwrap();
        assert_eq!(
            find_runner_dir(&app.join("MacOS")).unwrap(),
            app.join("Resources/upscaler")
        );
    }

    #[test]
    fn free_space_is_checked() {
        let d = tempfile::tempdir().unwrap();
        check_space(d.path(), 1).unwrap();
        match check_space(d.path(), u64::MAX) {
            Err(UpscaleError::NoSpace { needed, available }) => {
                assert_eq!(needed, u64::MAX);
                assert!(available < needed);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(FREE_SPACE_NEEDED, 3584 << 20);
    }

    #[test]
    fn create_no_window_flag() {
        // The documented value of CREATE_NO_WINDOW (checked against
        // windows-sys at compile time on Windows).
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }
}
