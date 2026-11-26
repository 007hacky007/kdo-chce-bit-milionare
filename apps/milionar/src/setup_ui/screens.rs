//! The wizard's pages as display lists, and the texts of import errors.

use render::{Font, Frame};
use setup::ImportError;

use super::widgets::{ACCENT, DIM, Page as Builder, TEXT, WARN};
use super::{Note, Page, Wizard};
use crate::app::SetupReason;
use crate::strings::{Lang, Str, fill, text};

/// What to tell the player about a failed import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorText {
    /// Paragraphs: what happened first, then what to do.
    pub lines: Vec<String>,
    /// Technical details (shown small after "Details:"), if any.
    pub details: Option<String>,
    /// Trying the same source again can work (after the player acts).
    pub retry: bool,
}

/// Bytes as "612 MB" or "3,5 GB" (binary units, rounded up to whole MB).
pub fn human_size(lang: Lang, bytes: u64) -> String {
    const MB: u64 = 1 << 20;
    let mb = bytes.div_ceil(MB);
    if mb < 1024 {
        return format!("{mb} MB");
    }
    let gb = format!("{:.1}", mb as f64 / 1024.0);
    let gb = gb.strip_suffix(".0").unwrap_or(&gb).to_string();
    match lang {
        Lang::Cs => format!("{} GB", gb.replace('.', ",")),
        Lang::En => format!("{gb} GB"),
    }
}

/// A hash or long value cut to something readable.
fn short(v: &str) -> String {
    if v.chars().count() > 20 {
        format!("{}...", v.chars().take(12).collect::<String>())
    } else {
        v.to_string()
    }
}

/// The two sides of a mismatch: sizes ("N bytes" from the importer) as MB, or as bytes
/// when they round to the same MB; hashes shortened.
fn mismatch_values(lang: Lang, expected: &str, actual: &str) -> (String, String) {
    let bytes = |v: &str| v.strip_suffix(" bytes").and_then(|n| n.parse::<u64>().ok());
    if let (Some(e), Some(a)) = (bytes(expected), bytes(actual)) {
        let (he, ha) = (human_size(lang, e), human_size(lang, a));
        return if he == ha {
            (format!("{e} B"), format!("{a} B"))
        } else {
            (he, ha)
        };
    }
    (short(expected), short(actual))
}

/// A plain-language reason for the common I/O errors, `None` for the others.
fn io_reason(lang: Lang, e: &std::io::Error) -> Option<&'static str> {
    use std::io::ErrorKind as K;
    let s = match e.kind() {
        K::NotFound => Str::IoNotFound,
        K::PermissionDenied => Str::IoDenied,
        K::StorageFull => Str::IoFull,
        K::ReadOnlyFilesystem => Str::IoReadOnly,
        _ => return None,
    };
    Some(text(lang, s))
}

/// The player's explanation of an import error (every variant).
pub fn error_text(lang: Lang, e: &ImportError) -> ErrorText {
    let t = |s| text(lang, s);
    let size = |b| human_size(lang, b);
    let mut details = None;
    let (lines, retry) = match e {
        ImportError::NotRecognized(p) => (
            vec![
                fill(
                    t(Str::ErrNotRecognized),
                    &[("path", &p.display().to_string())],
                ),
                t(Str::ErrNotRecognizedHint).to_string(),
            ],
            false,
        ),
        ImportError::Mismatch {
            file,
            expected,
            actual,
        } => (
            vec![
                fill(t(Str::ErrMismatch), &[("file", file)]),
                {
                    let (expected, actual) = mismatch_values(lang, expected, actual);
                    fill(
                        t(Str::ErrMismatchDetail),
                        &[("expected", &expected), ("actual", &actual)],
                    )
                },
                t(Str::ErrMismatchHint).to_string(),
            ],
            false,
        ),
        ImportError::Missing(file) => (
            vec![
                fill(t(Str::ErrMissing), &[("file", file)]),
                t(Str::ErrMissingHint).to_string(),
            ],
            false,
        ),
        ImportError::NoSpace { needed, available } => (
            vec![
                fill(
                    t(Str::ErrNoSpace),
                    &[("needed", &size(*needed)), ("available", &size(*available))],
                ),
                t(Str::ErrNoSpaceHint).to_string(),
            ],
            true,
        ),
        ImportError::Busy(_) => (vec![t(Str::ErrBusy).to_string()], true),
        ImportError::GameRunning(_) => (vec![t(Str::ErrGameRunning).to_string()], true),
        ImportError::Cancelled => (vec![t(Str::ErrCancelled).to_string()], false),
        ImportError::Io { path, source } => {
            let path = path.display().to_string();
            match io_reason(lang, source) {
                Some(reason) => {
                    let mut lines =
                        vec![fill(t(Str::ErrIo), &[("path", &path), ("reason", reason)])];
                    let denied = source.kind() == std::io::ErrorKind::PermissionDenied;
                    if denied {
                        lines.push(t(Str::ErrIoDeniedHint).to_string());
                    }
                    (lines, denied)
                }
                None => {
                    details = Some(source.to_string());
                    (vec![fill(t(Str::ErrIoOther), &[("path", &path)])], false)
                }
            }
        }
        ImportError::Format { path, source } => {
            details = Some(source.to_string());
            (
                vec![fill(
                    t(Str::ErrFormat),
                    &[("path", &path.display().to_string())],
                )],
                false,
            )
        }
        ImportError::Incomplete {
            path,
            actual,
            expected,
        } => (
            vec![fill(
                t(Str::ErrIncomplete),
                &[
                    ("path", &path.display().to_string()),
                    ("actual", &size(*actual)),
                    ("expected", &size(*expected)),
                ],
            )],
            false,
        ),
        ImportError::ForeignGameDir(p) => (
            vec![fill(
                t(Str::ErrForeign),
                &[("path", &p.display().to_string())],
            )],
            true,
        ),
    };
    ErrorText {
        lines,
        details,
        retry,
    }
}

/// Draws the wizard's current page.
pub fn frame(w: &Wizard, font: &Font) -> Frame {
    let lang = w.lang;
    let t = |s| text(lang, s);
    let actions = w.actions();
    let title = match &w.page {
        Page::Welcome => Str::WelcomeTitle,
        Page::Waiting => Str::WaitTitle,
        Page::Choose => Str::ChooseTitle,
        Page::Importing { .. } => Str::ImportTitle,
        Page::Failed { .. } => Str::FailTitle,
        Page::Done { .. } => Str::DoneTitle,
        Page::Upscale { .. } => Str::UpscaleTitle,
        Page::Upscaling { .. } => Str::RunTitle,
        Page::UpscaleCpu { .. } => Str::CpuTitle,
        Page::UpscaleFailed { .. } => Str::UpFailTitle,
        Page::UpscaleDone { .. } => Str::UpDoneTitle,
    };
    let mut p = Builder::new(font, t(title), actions.len());
    match &w.page {
        Page::Welcome => {
            if let SetupReason::Unreadable(why) = &w.reason {
                p.para(t(Str::WelcomeUnreadable), WARN);
                p.small(&fill(t(Str::Details), &[("reason", why)]), DIM);
            }
            p.para(t(Str::WelcomeNeed), TEXT);
            p.para(t(Str::WelcomeAlt), TEXT);
            p.small(t(Str::WelcomeSpace), DIM);
        }
        Page::Waiting => {
            p.para(t(Str::WaitBody), TEXT);
            if w.note == Some(Note::StillRunning) {
                p.para(t(Str::WaitStill), WARN);
            }
        }
        Page::Choose => {
            p.para(t(Str::ChooseBody), TEXT);
            p.small(t(Str::ChooseDrop), DIM);
            match &w.note {
                Some(Note::NotRecognized(path)) => {
                    let msg = fill(
                        t(Str::ErrNotRecognized),
                        &[("path", &path.display().to_string())],
                    );
                    p.para(&msg, WARN);
                }
                Some(Note::DialogFailed(why)) => {
                    p.para(t(Str::DialogFailed), WARN);
                    p.small(&fill(t(Str::Details), &[("reason", why)]), DIM);
                }
                Some(Note::Cancelled) => p.para(t(Str::NoteCancelled), ACCENT),
                Some(Note::StillRunning) | None => {}
            }
        }
        Page::Importing {
            source,
            progress,
            cancelling,
        } => {
            let path = source.path().display().to_string();
            p.small(&fill(t(Str::ImportSource), &[("path", &path)]), DIM);
            match progress {
                None => {
                    p.bar(0.0);
                    p.para(t(Str::ImportPreparing), TEXT);
                }
                Some(pr) => {
                    let total = pr.total_bytes.max(1);
                    p.bar(pr.done_bytes as f64 / total as f64);
                    let amount = fill(
                        t(Str::ImportAmount),
                        &[
                            ("done", &human_size(lang, pr.done_bytes)),
                            ("total", &human_size(lang, pr.total_bytes)),
                        ],
                    );
                    p.para(&amount, TEXT);
                    p.small(&fill(t(Str::ImportFile), &[("file", pr.current)]), DIM);
                }
            }
            if *cancelling {
                p.para(t(Str::ImportCancelling), WARN);
            } else {
                p.small(t(Str::ImportKeepsData), DIM);
            }
        }
        Page::Failed { text: e, .. } => {
            for (i, line) in e.lines.iter().enumerate() {
                p.para(line, if i == 0 { WARN } else { TEXT });
            }
            if let Some(d) = &e.details {
                p.small(&fill(t(Str::Details), &[("reason", d)]), DIM);
            }
            p.small(t(Str::FailKeepsData), DIM);
        }
        Page::Done { report, .. } => {
            let msg = fill(
                t(Str::DoneBody),
                &[
                    ("files", &report.files.to_string()),
                    ("size", &human_size(lang, report.bytes)),
                ],
            );
            p.para(&msg, TEXT);
        }
        Page::Upscale { ctx, cancelled } => {
            use crate::art::PackKind;
            // A rebuild of a pack that exists: what happens to it, not what it is.
            let again = ctx.rebuild && ctx.pack != PackKind::Missing;
            if *cancelled {
                p.para(t(Str::UpscaleCancelled), ACCENT);
            } else if ctx.pack == PackKind::Stale {
                p.para(t(Str::UpscaleStale), WARN);
            } else if again {
                p.para(t(Str::UpscaleRebuild), TEXT);
            }
            if !again {
                p.para(t(Str::UpscaleWhat), TEXT);
            }
            p.para(t(Str::UpscaleTime), TEXT);
            p.small(t(Str::UpscaleDisk), DIM);
            if !*cancelled {
                p.small(t(Str::UpscaleCancelKeeps), DIM);
                if !ctx.rebuild {
                    p.small(t(Str::UpscaleLater), DIM);
                }
            }
        }
        Page::Upscaling {
            status, cancelling, ..
        } => super::upscale::draw_running(&mut p, lang, status.as_ref(), *cancelling),
        Page::UpscaleCpu { device, .. } => {
            p.para(&fill(t(Str::CpuBody), &[("device", device)]), WARN);
            p.para(t(Str::CpuChoice), TEXT);
        }
        Page::UpscaleFailed { text: e, ctx } => {
            for (i, line) in e.lines.iter().enumerate() {
                p.para(line, if i == 0 { WARN } else { TEXT });
            }
            if let Some(d) = &e.details {
                p.small(&fill(t(Str::Details), &[("reason", d)]), DIM);
            }
            // A failed rebuild leaves a valid pack as it was (it is replaced only at
            // the very end).
            p.small(
                t(if ctx.pack == crate::art::PackKind::Ready {
                    Str::UpFailKept
                } else {
                    Str::UpFailOriginal
                }),
                DIM,
            );
        }
        Page::UpscaleDone { report } => {
            p.para(t(Str::UpDoneBody), TEXT);
            if report.up_to_date {
                p.small(t(Str::UpDoneCurrent), DIM);
            }
        }
    }
    let labels: Vec<&str> = actions.iter().map(|a| w.label(*a)).collect();
    // What Esc does on this page.
    let hint = match &w.page {
        Page::Welcome | Page::Waiting => Str::HintQuit,
        Page::Importing { .. } | Page::Upscaling { .. } => Str::HintCancel,
        Page::Done { .. } | Page::UpscaleDone { .. } => Str::HintNoEsc,
        _ if w.esc_starts_game() && w.opened_from_game() => Str::HintBackToGame,
        _ if w.esc_starts_game() => Str::HintStartGame,
        _ => Str::HintBack,
    };
    p.finish(&labels, w.focus, t(hint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::art::PackKind;
    use crate::setup_ui::{Action, Nav, UpscaleFail, Wizard, widgets};
    use crate::strings::Lang;
    use render::{Compositor, Font, TextLayout, layout_text};
    use setup::{ImportError, ImportReport, Progress, Source};
    use std::path::PathBuf;

    fn all_errors() -> Vec<ImportError> {
        let long = PathBuf::from(
            "/Users/someone/Library/Application Support/Kdo chce bít milionáře/a very long folder name/Kdo chce bít milionáře (CZ).iso",
        );
        vec![
            ImportError::NotRecognized(long.clone()),
            ImportError::Mismatch {
                file: "Cuts/Logos.smk".into(),
                expected: "a".repeat(64),
                actual: "b".repeat(64),
            },
            ImportError::Missing("Audiolib/Zvuky.dat".into()),
            ImportError::NoSpace {
                needed: 700 << 20,
                available: 3 << 30,
            },
            ImportError::Busy(long.clone()),
            ImportError::GameRunning(long.clone()),
            ImportError::Cancelled,
            ImportError::Io {
                path: long.clone(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
            ImportError::Io {
                path: long.clone(),
                source: std::io::Error::other("strange failure"),
            },
            ImportError::Io {
                path: long.clone(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
            ImportError::Format {
                path: long.clone(),
                source: formats::FormatError::Invalid {
                    what: "udf anchor",
                    detail: "bad tag".into(),
                },
            },
            ImportError::Incomplete {
                path: long.clone(),
                actual: 100 << 20,
                expected: 650 << 20,
            },
            ImportError::ForeignGameDir(long),
        ]
    }

    #[test]
    fn mismatch_sizes_are_localised() {
        let m = |e: &str, a: &str| mismatch_values(Lang::Cs, e, a);
        assert_eq!(
            m("341048406 bytes", "4096 bytes"),
            ("326 MB".to_string(), "1 MB".to_string())
        );
        assert_eq!(
            m("341048406 bytes", "341048000 bytes"),
            ("341048406 B".to_string(), "341048000 B".to_string())
        );
        let h = "0123456789abcdef".repeat(4);
        assert_eq!(m(&h, "abc").0, "0123456789ab...");
    }

    /// Every string of the table, in both languages and with long values filled in,
    /// fits as a paragraph on a page with three buttons (nothing is cut short).
    #[test]
    fn every_string_fits_a_page() {
        let font = Font::bundled();
        let path = "/Users/someone/Library/Application Support/Kdo chce bít milionáře/a long folder/Kdo chce bít milionáře (CZ).iso";
        let values = [
            ("path", path),
            ("file", "Cuts/A very long video file name.smk"),
            (
                "reason",
                "cannot index /Users/someone/Library/Application Support/milionar-port/Milionar/game: Permission denied (os error 13)",
            ),
            ("needed", "1,5 GB"),
            ("available", "600 MB"),
            ("expected", "0123456789ab..."),
            ("actual", "0123456789ab..."),
            ("done", "300 MB"),
            ("total", "326 MB"),
            ("files", "312"),
            ("size", "325 MB"),
        ];
        for lang in [Lang::Cs, Lang::En] {
            for &s in crate::strings::ALL {
                let t = fill(text(lang, s), &values);
                let mut p = Builder::new(&font, text(lang, Str::FailTitle), 3);
                p.para(&t, TEXT);
                let frame = p.finish(&["a", "b", "c"], 0, "hint");
                for op in &frame.ops {
                    if let render::DrawOp::Text { text, .. } = op {
                        assert!(
                            !text.contains(widgets::ELLIPSIS),
                            "{lang:?} {s:?} cut short"
                        );
                    }
                }
                assert!(
                    frame
                        .ops
                        .iter()
                        .any(|op| matches!(op, render::DrawOp::Text { text, .. } if *text == t)),
                    "{lang:?} {s:?} not drawn"
                );
            }
        }
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(human_size(Lang::Cs, 600 << 20), "600 MB");
        assert_eq!(human_size(Lang::En, 600 << 20), "600 MB");
        assert_eq!(human_size(Lang::Cs, 3584 << 20), "3,5 GB");
        assert_eq!(human_size(Lang::En, 3584 << 20), "3.5 GB");
        assert_eq!(human_size(Lang::En, 1000), "1 MB");
        assert_eq!(human_size(Lang::En, 0), "0 MB");
    }

    #[test]
    fn error_messages_cover_all_import_errors() {
        for lang in [Lang::Cs, Lang::En] {
            for e in all_errors() {
                let t = error_text(lang, &e);
                let all = t.lines.join("\n");
                // Technical text only in the details line, for the errors that have some.
                let technical = match &e {
                    ImportError::Format { .. } => true,
                    ImportError::Io { source, .. } => io_reason(lang, source).is_none(),
                    _ => false,
                };
                assert_eq!(t.details.is_some(), technical, "{e:?}");
                assert!(
                    !all.contains("bad tag") && !all.contains("strange failure"),
                    "{all}"
                );
                assert!(
                    !t.lines.is_empty() && t.lines.iter().all(|l| !l.is_empty()),
                    "{e:?}"
                );
                assert!(!all.contains('{'), "unfilled placeholder: {all}");
                let retry = match &e {
                    ImportError::Busy(_)
                    | ImportError::GameRunning(_)
                    | ImportError::NoSpace { .. }
                    | ImportError::ForeignGameDir(_) => true,
                    ImportError::Io { source, .. } => {
                        source.kind() == std::io::ErrorKind::PermissionDenied
                    }
                    _ => false,
                };
                assert_eq!(t.retry, retry, "{e:?}");
                match &e {
                    ImportError::NotRecognized(p)
                    | ImportError::Io { path: p, .. }
                    | ImportError::Format { path: p, .. }
                    | ImportError::Incomplete { path: p, .. }
                    | ImportError::ForeignGameDir(p) => {
                        assert!(all.contains(&p.display().to_string()), "{all}")
                    }
                    ImportError::Mismatch { file, .. } | ImportError::Missing(file) => {
                        assert!(all.contains(file.as_str()), "{all}")
                    }
                    ImportError::NoSpace { .. } => {
                        assert!(all.contains("700 MB") && all.contains(" GB"), "{all}")
                    }
                    ImportError::Busy(_) | ImportError::GameRunning(_) | ImportError::Cancelled => {
                    }
                }
                if let ImportError::Incomplete { .. } = e {
                    assert!(all.contains("100 MB") && all.contains("650 MB"), "{all}");
                }
                if let ImportError::Mismatch { .. } = e {
                    assert!(
                        !all.contains(&"a".repeat(40)),
                        "hashes are shortened: {all}"
                    );
                }
                if let (Lang::Cs, ImportError::Io { source, .. }) = (lang, &e)
                    && source.kind() == std::io::ErrorKind::NotFound
                {
                    assert!(all.contains("neexistuje"), "{all}");
                }
            }
        }
    }

    /// Every page in both languages, with long paths: drawn without panicking, every text
    /// line inside its own rectangle and the panel, no text over a button.
    #[test]
    fn every_screen_renders_inside_its_panel() {
        let font = Font::bundled();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join("Kdo chce bít milionáře - no, a very long install folder name here");
        std::fs::create_dir_all(dir.join("Audiolib")).unwrap();
        std::fs::write(dir.join("Milionar.exe"), b"x").unwrap();
        let long_reason = "cannot index /Users/someone/Library/Application Support/milionar-port/Milionar/game/Menus: Permission denied (os error 13)".to_string();
        let mut states: Vec<(String, Wizard)> = Vec::new();
        for lang in [Lang::Cs, Lang::En] {
            let mut add = |name: &str, w: Wizard| states.push((format!("{lang:?} {name}"), w));
            add("welcome", Wizard::new(lang, SetupReason::NoImport));
            add(
                "welcome unreadable",
                Wizard::new(lang, SetupReason::Unreadable(long_reason.clone())),
            );
            add("waiting", Wizard::new(lang, SetupReason::ImportRunning));
            let mut w = Wizard::new(lang, SetupReason::ImportRunning);
            w.restart(SetupReason::ImportRunning);
            add("waiting still", w);
            let mut w = Wizard::new(lang, SetupReason::NoImport);
            w.key(Nav::Activate);
            add("choose", w.clone());
            w.choose(
                &tmp.path()
                    .join("README with a rather long name that goes on and on.md"),
            );
            add("choose not recognized", w.clone());
            w.dialog_failed(long_reason.clone());
            add("choose dialog failed", w.clone());
            w.choose(&dir);
            add("importing preparing", w.clone());
            w.progress(Progress {
                done_bytes: 300 << 20,
                total_bytes: 620 << 20,
                current: "Cuts/A very long video file name.smk",
            });
            add("importing", w.clone());
            w.key(Nav::Back);
            add("importing cancelling", w.clone());
            w.finished(Err(ImportError::Cancelled), PackKind::Missing);
            add("choose cancelled", w.clone());
            for (i, e) in all_errors().into_iter().enumerate() {
                let mut f = w.clone();
                f.choose(&dir);
                f.finished(Err(e), PackKind::Missing);
                add(&format!("failed {i}"), f);
            }
            let report = ImportReport {
                source: Source::Installed(dir.clone()),
                files: 312,
                bytes: 612 << 20,
            };
            let mut d = w.clone();
            d.choose(&dir);
            d.finished(Ok(report.clone()), PackKind::Missing);
            add("done", d.clone());
            d.key(Nav::Activate);
            add("upscale", d);
            let mut st = w.clone();
            st.choose(&dir);
            st.finished(Ok(report.clone()), PackKind::Stale);
            st.key(Nav::Activate);
            add("upscale stale", st.clone());
            add("upscale build", Wizard::new(lang, SetupReason::Rebuild));
            for pack in [PackKind::Ready, PackKind::Stale] {
                let mut r = Wizard::new(lang, SetupReason::Rebuild);
                r.set_offer_pack(pack);
                add(&format!("upscale rebuild {pack:?}"), r.clone());
                r.key(Nav::Activate);
                r.upscale_finished(Err(UpscaleFail::Error(
                    upscale::UpscaleError::RunnerFailed {
                        code: 1,
                        stderr: long_reason.clone(),
                    },
                )));
                add(&format!("upscale rebuild failed {pack:?}"), r);
            }
            add("reimport choose", Wizard::new(lang, SetupReason::Reimport));
            let mut ow = w.clone();
            ow.reimport();
            add("reimport from the wizard", ow.clone());
            ow.rebuild(PackKind::Ready);
            add("rebuild from the wizard", ow);
            st.key(Nav::Activate);
            add("upscaling checking", st.clone());
            let prog = |phase, done, total| upscale::Progress {
                phase,
                done,
                total,
                elapsed: std::time::Duration::from_secs(200),
                eta: Some(std::time::Duration::from_secs(3 * 3600 + 900)),
            };
            let mut status = crate::setup_ui::UpscaleStatus {
                device: Some("NVIDIA GeForce RTX 4090 Laptop GPU with a long name".into()),
                ..Default::default()
            };
            st.upscale_status(status.clone());
            add("upscaling device", st.clone());
            status.phases[0] = Some(prog(upscale::Phase::Scan, 88, 88));
            status.phases[1] = Some(prog(upscale::Phase::Upscale, 1234, 2450));
            st.upscale_status(status.clone());
            add("upscaling", st.clone());
            status.phases[2] = Some(prog(upscale::Phase::Assemble, 3000, 3235));
            st.upscale_status(status);
            add("upscaling saving", st.clone());
            let mut c = st.clone();
            c.key(Nav::Back);
            add("upscaling cancelling", c.clone());
            c.upscale_finished(Err(UpscaleFail::Error(upscale::UpscaleError::Cancelled)));
            add("upscale cancelled", c);
            let mut c = st.clone();
            c.upscale_finished(Err(UpscaleFail::CpuOnly(
                "llvmpipe (LLVM 15.0.7, 256 bits)".into(),
            )));
            add("upscale cpu", c);
            for (i, f) in [
                UpscaleFail::NoRunner(vec![dir.clone()]),
                UpscaleFail::Error(upscale::UpscaleError::NoVulkan {
                    stderr: long_reason.clone(),
                }),
                UpscaleFail::Error(upscale::UpscaleError::NoSpace {
                    needed: 3584 << 20,
                    available: 100 << 20,
                }),
                UpscaleFail::Error(upscale::UpscaleError::RunnerFailed {
                    code: 1,
                    stderr: long_reason.clone(),
                }),
            ]
            .into_iter()
            .enumerate()
            {
                let mut c = st.clone();
                c.upscale_finished(Err(f));
                add(&format!("upscale failed {i}"), c);
            }
            let mut c = st.clone();
            c.upscale_finished(Ok(upscale::Report {
                up_to_date: true,
                ..Default::default()
            }));
            add("upscale done", c);
            let mut d = w.clone();
            d.choose(&dir);
            d.finished(Ok(report), PackKind::Ready);
            add("done with pack", d);
        }
        let mut comp = Compositor::new(2, Font::bundled());
        for (name, w) in &states {
            let frame = w.frame(&font);
            let buttons = widgets::button_rects(w.actions().len());
            let mut texts = 0;
            for op in &frame.ops {
                let render::DrawOp::Text {
                    text,
                    font: spec,
                    rect,
                    layout,
                    ..
                } = op
                else {
                    continue;
                };
                texts += 1;
                assert!(!text.is_empty(), "{name}");
                assert!(
                    !text.contains(widgets::ELLIPSIS),
                    "{name}: cut short: {text}"
                );
                let laid = layout_text(&font, text, *spec, *rect, *layout);
                let cell = laid.scale.cell_height();
                for l in &laid.lines {
                    let inside = |r: &render::Rect| {
                        l.x >= r.left
                            && l.x + l.width <= r.right
                            && l.y >= r.top
                            && l.y + cell <= r.bottom
                    };
                    assert!(
                        inside(rect),
                        "{name}: {:?} outside its rect {rect:?}",
                        l.text
                    );
                    assert!(
                        inside(&widgets::PANEL),
                        "{name}: {:?} outside the panel",
                        l.text
                    );
                }
                let is_label = buttons.contains(rect);
                if *layout == TextLayout::WrappedLeft && !is_label {
                    for b in &buttons {
                        let overlap = rect.left < b.right
                            && b.left < rect.right
                            && rect.top < b.bottom
                            && b.top < rect.bottom;
                        assert!(!overlap, "{name}: {text:?} over a button");
                    }
                }
            }
            assert!(texts >= 3, "{name}: title, body and buttons");
            for (i, a) in w.actions().iter().enumerate() {
                let label = w.label(*a);
                assert!(
                    frame.ops.iter().any(|op| matches!(op, render::DrawOp::Text { text, rect, .. } if *text == label && *rect == buttons[i])),
                    "{name}: button {a:?}"
                );
            }
            let img = comp.compose(&frame, &engine::FrameSnapshot::default());
            let px = |x: u32, y: u32| {
                let i = ((y * img.width() + x) * 4) as usize;
                [img.as_raw()[i], img.as_raw()[i + 1], img.as_raw()[i + 2]]
            };
            assert_eq!(px(2, 2), widgets::BG, "{name}");
            assert_eq!(px(600, 30 * 2), widgets::PANEL_BG, "{name}");
        }
        let _ = Action::Quit;
    }
}
