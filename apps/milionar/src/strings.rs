//! Text of the port's own screens (setup wizard, later the options overlay), all in one
//! table with a Czech and an English entry per key.
//!
//! The language follows the system: Czech for a Czech or Slovak locale, else English.
//! These are the port's own words, never text from the game. `{name}` marks a value
//! filled in with [`fill`].

/// The language of the port's own screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Cs,
    En,
}

impl Lang {
    /// From preferred locales, most preferred first (`cs`, `sk_SK`, `en-US`, ...): Czech
    /// when the first one is Czech or Slovak.
    pub fn from_locales<'a>(locales: impl IntoIterator<Item = &'a str>) -> Lang {
        let first = locales.into_iter().next().unwrap_or("");
        let code: String = first
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_lowercase();
        if code == "cs" || code == "sk" {
            Lang::Cs
        } else {
            Lang::En
        }
    }

    /// The system language (call after SDL is initialised). With the `test-hooks`
    /// feature, `MILIONAR_LANG=cs|en` overrides it.
    pub fn system() -> Lang {
        #[cfg(feature = "test-hooks")]
        if let Ok(v) = std::env::var("MILIONAR_LANG") {
            return Lang::from_locales([v.as_str()]);
        }
        let preferred = preferred_languages();
        if !preferred.is_empty() {
            return Lang::from_locales(preferred.iter().map(String::as_str));
        }
        let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        Lang::from_locales([env.as_str()])
    }
}

/// The user's preferred UI languages from SDL (macOS and Windows system settings).
fn preferred_languages() -> Vec<String> {
    use sdl3::sys::locale::SDL_GetPreferredLocales;
    let mut out = Vec::new();
    let mut count: std::ffi::c_int = 0;
    // SAFETY: SDL returns null or an array of `count` valid locale pointers (each with a
    // NUL-terminated language string), allocated as one block freed with SDL_free.
    unsafe {
        let list = SDL_GetPreferredLocales(&mut count);
        if list.is_null() {
            return out;
        }
        for i in 0..usize::try_from(count).unwrap_or(0) {
            let loc = *list.add(i);
            if loc.is_null() || (*loc).language.is_null() {
                continue;
            }
            let lang = std::ffi::CStr::from_ptr((*loc).language);
            out.push(lang.to_string_lossy().into_owned());
        }
        sdl3::sys::stdinc::SDL_free(list.cast());
    }
    out
}

/// `template` with every `{name}` from `values` replaced (one pass: inserted values are
/// not searched again). Unknown names stay as they are.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').and_then(|close| {
            values
                .iter()
                .find(|(k, _)| *k == &after[..close])
                .map(|v| (close, v.1))
        }) {
            Some((close, value)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

macro_rules! table {
    ($($(#[$doc:meta])* $key:ident => $cs:literal, $en:literal;)*) => {
        /// A key of the table.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Str {
            $($(#[$doc])* $key,)*
        }

        /// Every key, for completeness checks.
        #[cfg(test)]
        pub const ALL: &[Str] = &[$(Str::$key,)*];

        /// The text of `s` in `lang`.
        pub fn text(lang: Lang, s: Str) -> &'static str {
            let (cs, en) = match s {
                $(Str::$key => ($cs, $en),)*
            };
            match lang {
                Lang::Cs => cs,
                Lang::En => en,
            }
        }
    };
}

table! {
    BtnContinue => "Pokračovat", "Continue";
    BtnQuit => "Ukončit", "Quit";
    BtnBack => "Zpět", "Back";
    BtnCancel => "Zrušit import", "Cancel the import";
    BtnRetry => "Zkusit znovu", "Try again";
    BtnPickFile => "Vybrat obraz CD (soubor .iso) nebo Setup.exe",
        "Choose a disc image (.iso file) or Setup.exe";
    BtnPickFolder => "Vybrat CD nebo složku", "Choose the CD or a folder";
    BtnOtherSource => "Vybrat jiný zdroj", "Choose another source";
    BtnStartGame => "Spustit hru", "Start the game";
    HintQuit => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit   Esc: ukončit",
        "Arrows and Tab: move   Enter: select   Esc: quit";
    HintCancel => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit   Esc: zrušit",
        "Arrows and Tab: move   Enter: select   Esc: cancel";
    HintBack => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit   Esc: zpět",
        "Arrows and Tab: move   Enter: select   Esc: back";
    HintNoEsc => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit", "Arrows and Tab: move   Enter: select";
    HintBackToGame => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit   Esc: zpět do hry",
        "Arrows and Tab: move   Enter: select   Esc: back to the game";
    HintStartGame => "Šipky a\u{a0}Tab: přesun   Enter: potvrdit   Esc: spustit hru",
        "Arrows and Tab: move   Enter: select   Esc: start the game";
    Details => "Podrobnosti: {reason}", "Details: {reason}";

    WelcomeTitle => "Vítejte", "Welcome";
    WelcomeNeed => "Ke hře potřebujete vlastní originální CD (vydání TopCD). Hra z\u{a0}něj jednou zkopíruje svá data (asi 350 MB), potom už CD nebudete potřebovat.",
        "You need your own original CD (the TopCD release). The game copies its data from it once (about 350 MB); after that you no longer need the CD.";
    WelcomeAlt => "Místo CD můžete použít obraz CD (soubor .iso), soubor Setup.exe z\u{a0}CD nebo složku, kam jste hru dříve nainstalovali.",
        "Instead of the CD you can use a disc image (.iso file), the Setup.exe file from the CD, or a folder where you installed the game before.";
    WelcomeSpace => "Na disku počítače potřebujete asi 600 MB volného místa.",
        "You need about 600 MB of free space on your computer's disk.";
    WelcomeUnreadable => "Dříve importovaná data nelze přečíst. Je potřeba je importovat znovu.",
        "The previously imported data cannot be read. It has to be imported again.";

    WaitTitle => "Probíhá jiný import", "Another import is running";
    WaitBody => "Data hry právě importuje jiné okno této aplikace. Počkejte, až skončí, a\u{a0}pak zvolte Zkusit znovu.",
        "Another window of this app is importing the game data right now. Wait until it finishes, then choose Try again.";
    WaitStill => "Import pořád probíhá.", "The import is still running.";

    ChooseTitle => "Odkud importovat?", "Where to import from?";
    ChooseBody => "Vyberte obraz CD (soubor .iso), soubor Setup.exe, vložené CD nebo složku s\u{a0}nainstalovanou hrou.",
        "Choose a disc image (.iso file), the Setup.exe file, the inserted CD or a folder with the installed game.";
    ChooseDrop => "Soubor nebo složku můžete také přetáhnout do tohoto okna.",
        "You can also drag a file or folder onto this window.";
    FilterSource => "Obraz CD (soubor .iso) nebo Setup.exe", "Disc image (.iso file) or Setup.exe";
    FilterAll => "Všechny soubory", "All files";
    DialogFailed => "Okno pro výběr souboru nejde otevřít. Přetáhněte soubor nebo složku do tohoto okna.",
        "The file dialog cannot be opened. Drag the file or folder onto this window instead.";
    NoteCancelled => "Import byl zrušen. Dosavadní data zůstala beze změny.",
        "The import was cancelled. Existing data was left unchanged.";

    ImportTitle => "Probíhá import dat hry", "Importing the game data";
    ImportSource => "Zdroj: {path}", "Source: {path}";
    ImportFile => "Soubor: {file}", "File: {file}";
    ImportAmount => "{done} z\u{a0}{total}", "{done} of {total}";
    ImportPreparing => "Připravuji import...", "Preparing the import...";
    ImportCancelling => "Ruším import...", "Cancelling the import...";
    ImportKeepsData => "Při zrušení zůstanou dosavadní data beze změny.",
        "Cancelling leaves existing data unchanged.";

    DoneTitle => "Import dokončen", "Import complete";
    DoneBody => "Data hry jsou připravena ({files} souborů, {size}). CD už nebudete potřebovat.",
        "The game data is ready ({files} files, {size}). You no longer need the CD.";

    BtnOptions => "Nastavení", "Options";
    BtnRunUpscale => "Vytvořit vylepšenou grafiku", "Make the enhanced graphics";
    BtnSkipUpscale => "Hrát s\u{a0}původní grafikou", "Play with the original graphics";
    BtnCancelUpscale => "Zrušit vytváření", "Cancel";
    BtnRunAnyway => "Přesto spustit", "Start anyway";
    BtnBackToGame => "Zpět do hry", "Back to the game";

    UpscaleTitle => "Vylepšená grafika", "Enhanced graphics";
    UpscaleWhat => "Hra umí obrázky a\u{a0}videa zobrazit ve trojnásobném rozlišení. Vylepšenou grafiku jednou vytvoří umělá inteligence přímo ve vašem počítači.",
        "The game can show its pictures and videos at three times the resolution. An AI model makes the enhanced graphics once, on your computer.";
    UpscaleTime => "Na Macu s\u{a0}čipem Apple to trvá asi 8 minut, na jiných počítačích může trvat déle. Je potřeba grafická karta.",
        "It takes about 8 minutes on a Mac with Apple silicon and may take longer on other computers. A graphics card is needed.";
    UpscaleDisk => "Grafika zabere asi 1,1 GB. Během vytváření je potřeba asi 3,5 GB volného místa na disku počítače.",
        "The graphics take about 1.1 GB. About 3.5 GB of free space on your computer's disk is needed while they are made.";
    UpscaleCancelKeeps => "Vytváření můžete kdykoli zrušit, hotová část zůstane uložená.",
        "You can cancel at any time; the finished part is kept.";
    UpscaleLater => "Vylepšenou grafiku můžete vytvořit i\u{a0}později v\u{a0}nastavení (F10).",
        "You can also make the enhanced graphics later in the options (F10).";
    UpscaleStale => "Vylepšená grafika byla vytvořena z\u{a0}předchozího importu a\u{a0}k\u{a0}novým datům nepatří. Dokud ji nevytvoříte znovu, hra poběží s\u{a0}původní grafikou.",
        "The enhanced graphics were made from the previous import and do not match the new data. Until they are made again, the game uses the original graphics.";
    UpscaleRebuild => "Vylepšená grafika se vytvoří znovu z\u{a0}importovaných dat. Snímky, které se nezměnily, se převezmou z\u{a0}dosavadní grafiky.",
        "The enhanced graphics are made again from the imported data. Frames that did not change are taken from the existing graphics.";
    UpscaleCancelled => "Vytváření bylo zrušeno. Hotové snímky zůstaly uložené a\u{a0}příště se na ně naváže.",
        "Cancelled. The finished frames are kept, and the next run continues from them.";

    RunTitle => "Vytváří se vylepšená grafika", "Making the enhanced graphics";
    RunChecking => "Kontroluji grafickou kartu...", "Checking the graphics card...";
    RunDevice => "Grafická karta: {device}", "Graphics card: {device}";
    PhaseScan => "1. Procházení snímků", "1. Scanning the frames";
    PhaseUpscale => "2. Zvětšování snímků", "2. Upscaling the frames";
    PhaseAssemble => "3. Ukládání grafiky", "3. Saving the graphics";
    RunAmount => "{done} z\u{a0}{total}", "{done} of {total}";
    RunEta => "Zbývá asi {time}.", "About {time} left.";
    RunEtaUnknown => "Odhaduji zbývající čas...", "Estimating the time left...";
    RunCancelling => "Ruším vytváření...", "Cancelling...";
    RunKeeps => "Při zrušení zůstanou hotové snímky uložené.", "Cancelling keeps the finished frames.";
    Minutes => "{n}\u{a0}min", "{n} min";
    Seconds => "{n}\u{a0}s", "{n} s";
    Hours => "{h}\u{a0}h {m}\u{a0}min", "{h} h {m} min";

    CpuTitle => "Vytváření by bylo velmi pomalé", "This would be very slow";
    CpuBody => "Nebyla nalezena grafická karta, jen její náhrada v\u{a0}procesoru ({device}). Vytváření grafiky by tak trvalo několik hodin.",
        "No graphics card was found, only a software stand-in running on the processor ({device}). Making the graphics would take several hours.";
    CpuChoice => "Můžete hrát s\u{a0}původní grafikou, nebo vytváření přesto spustit.",
        "You can play with the original graphics, or start anyway.";

    UpFailTitle => "Vylepšenou grafiku se nepodařilo vytvořit", "The enhanced graphics could not be made";
    UpFailOriginal => "Hra poběží s\u{a0}původní grafikou.", "The game uses the original graphics.";
    UpFailKept => "Dosavadní vylepšená grafika zůstává beze změny.", "The existing enhanced graphics are kept as they were.";
    UpErrNoVulkan => "Pro vytváření grafiky je potřeba grafická karta s\u{a0}podporou Vulkan a\u{a0}ta nebyla nalezena.",
        "Making the graphics needs a graphics card with Vulkan support, and none was found.";
    UpErrNoVulkanHint => "Aktualizujte ovladač grafické karty a\u{a0}zkuste to znovu.",
        "Update the graphics card driver and try again.";
    UpErrTool => "Nástroj pro vytváření grafiky chybí nebo je poškozený.",
        "The tool that makes the graphics is missing or damaged.";
    UpErrToolHint => "Nainstalujte hru znovu.", "Reinstall the game.";
    UpErrCrashed => "Nástroj pro vytváření grafiky selhal.", "The tool that makes the graphics failed.";
    UpErrCrashedHint => "Zkuste to znovu. Hotové snímky zůstaly uložené.",
        "Try again. The finished frames are kept.";
    UpErrBusy => "Vylepšenou grafiku právě vytváří jiné okno této aplikace. Počkejte, až skončí, a\u{a0}zkuste to znovu.",
        "Another window of this app is making the enhanced graphics right now. Wait until it finishes, then try again.";
    UpErrGameRunning => "Hra běží v\u{a0}jiném okně. Nejdřív ji ukončete a\u{a0}pak zvolte Zkusit znovu.",
        "The game is running in another window. Quit it first, then choose Try again.";
    UpErrFiles => "Při práci se soubory grafiky došlo k\u{a0}chybě.", "An error occurred while writing the graphics files.";
    UpErrGame => "Data hry nelze přečíst.", "The game data cannot be read.";
    UpErrGameHint => "Importujte data hry znovu (v\u{a0}nastavení F10).", "Import the game data again (in the options, F10).";

    UpDoneTitle => "Vylepšená grafika je hotová", "The enhanced graphics are ready";
    UpDoneBody => "Hra ji teď použije. Mezi původní a\u{a0}vylepšenou grafikou můžete přepínat klávesou F8 nebo v\u{a0}nastavení (F10).",
        "The game uses them now. Switch between the original and the enhanced graphics with F8 or in the options (F10).";
    UpDoneCurrent => "Vylepšená grafika už byla aktuální.", "The enhanced graphics were already up to date.";

    OptTitle => "Nastavení", "Options";
    OptPaused => "Hra je pozastavená.", "The game is paused.";
    OptArt => "Grafika", "Graphics";
    OptArtUpscaled => "Vylepšená", "Enhanced";
    OptArtOriginal => "Původní", "Original";
    OptArtHelp => "Vylepšená grafika ve trojnásobném rozlišení, nebo původní obraz. Přepíná také klávesa F8.",
        "Enhanced graphics at three times the resolution, or the original picture. F8 switches too.";
    OptArtMissing => "Vylepšená grafika zatím není vytvořená. Vytvoříte ji volbou Vytvořit vylepšenou grafiku.",
        "The enhanced graphics have not been made yet. Choose Make the enhanced graphics below.";
    OptArtStale => "Vylepšená grafika patří k\u{a0}předchozímu importu. Vytvořte ji znovu volbou níže.",
        "The enhanced graphics belong to the previous import. Make them again with the option below.";
    OptArtUnusable => "Vylepšenou grafiku nelze použít. Vytvořte ji znovu volbou níže.",
        "The enhanced graphics cannot be used. Make them again with the option below.";
    OptScaling => "Zobrazení", "Scaling";
    OptScaleFit => "Přizpůsobit", "Fit";
    OptScaleInteger => "Celé násobky", "Whole multiples";
    OptScaleStretch => "Roztáhnout", "Stretch";
    OptScaleFitHelp => "Obraz co největší, se zachovaným poměrem stran.",
        "As large as possible, keeping the aspect ratio.";
    OptScaleIntegerHelp => "Jen celé násobky původní velikosti: nejostřejší obraz, kolem může zůstat okraj.",
        "Whole multiples of the original size only: the sharpest picture, possibly with a border around it.";
    OptScaleStretchHelp => "Obraz vyplní celé okno, i\u{a0}když se tím zdeformuje.",
        "The picture fills the whole window, even if that distorts it.";
    OptFullscreen => "Celá obrazovka", "Fullscreen";
    OptFullscreenHelp => "Přepíná také F11 nebo Alt+Enter.", "F11 or Alt+Enter switches too.";
    OptOn => "Zapnuto", "On";
    OptOff => "Vypnuto", "Off";
    OptVolume => "Hlasitost", "Volume";
    OptPercent => "{n}\u{a0}%", "{n}%";
    OptVolumeHelp => "Hlasitost všech zvuků hry. Mění se šipkami do stran.",
        "The volume of all game sounds. Change it with Left and Right.";
    OptReimport => "Znovu importovat data hry", "Import the game data again";
    OptReimportHelp => "Data hry se znovu zkopírují z\u{a0}CD, obrazu CD nebo složky.",
        "Copies the game data again from the CD, a disc image or a folder.";
    OptReimportHelpGame => "Data hry se znovu zkopírují z\u{a0}CD, obrazu CD nebo složky. Rozehraná hra skončí.",
        "Copies the game data again from the CD, a disc image or a folder. The current game ends.";
    OptRebuild => "Znovu vytvořit vylepšenou grafiku", "Make the enhanced graphics again";
    OptBuild => "Vytvořit vylepšenou grafiku", "Make the enhanced graphics";
    OptRebuildHelp => "Na Macu s\u{a0}čipem Apple to trvá asi 8 minut, jinde déle.",
        "Takes about 8 minutes on a Mac with Apple silicon, longer elsewhere.";
    OptRebuildHelpGame => "Na Macu s\u{a0}čipem Apple to trvá asi 8 minut, jinde déle. Rozehraná hra skončí.",
        "Takes about 8 minutes on a Mac with Apple silicon, longer elsewhere. The current game ends.";
    OptClose => "Zavřít", "Close";
    OptCloseHelpGame => "Hra pokračuje tam, kde se zastavila.", "The game continues where it stopped.";
    OptCloseHelp => "Zpět do průvodce.", "Back to the setup.";
    OptHint => "Nahoru a\u{a0}dolů: přesun   Do stran: změna   Esc: zavřít",
        "Up and Down: move   Left and Right: change   Esc: close";
    OptConfirmTitle => "Ukončit hru?", "End the game?";
    OptConfirmReimport => "Před novým importem dat musí hra skončit.",
        "The game has to end before the data is imported again.";
    OptConfirmRebuild => "Než se vylepšená grafika vytvoří znovu, musí hra skončit.",
        "The game has to end before the enhanced graphics are made again.";
    OptConfirmEnds => "Rozehraná hra se ukončí a\u{a0}nepůjde v\u{a0}ní pokračovat.",
        "The current game ends and cannot be continued.";
    OptConfirmYes => "Ukončit hru a\u{a0}pokračovat", "End the game and continue";

    FailTitle => "Import se nezdařil", "The import failed";
    FailKeepsData => "Dosavadní data zůstala beze změny.", "Existing data was left unchanged.";
    ErrNotRecognized => "{path} není obraz CD (soubor .iso), Setup.exe ani složka s\u{a0}hrou.",
        "{path} is not a disc image (.iso file), a Setup.exe or a game folder.";
    ErrNotRecognizedHint => "Vyberte obraz CD (soubor .iso), soubor Setup.exe z\u{a0}CD, samotné CD nebo složku s\u{a0}nainstalovanou hrou.",
        "Choose a disc image (.iso file), the Setup.exe from the CD, the CD itself or a folder with the installed game.";
    ErrMismatch => "Soubor {file} neodpovídá podporovanému vydání hry (TopCD).",
        "The file {file} does not match the supported release of the game (TopCD).";
    ErrMismatchDetail => "Očekáváno {expected}, nalezeno {actual}.", "Expected {expected}, found {actual}.";
    ErrMismatchHint => "CD může být poškozené nebo jde o\u{a0}jiné vydání hry.",
        "The CD may be damaged, or it is a different release of the game.";
    ErrMissing => "Ve zdroji chybí soubor {file}.", "The source is missing the file {file}.";
    ErrMissingHint => "Zkontrolujte, že jste vybrali celé CD nebo úplnou instalaci hry.",
        "Check that you chose the whole CD or a complete installation of the game.";
    ErrNoSpace => "Na disku počítače není dost místa: je potřeba {needed}, volných je {available}.",
        "Not enough space on your computer's disk: {needed} is needed, {available} is free.";
    ErrNoSpaceHint => "Uvolněte místo na disku a\u{a0}zkuste to znovu.", "Free some disk space and try again.";
    ErrBusy => "Jiné okno této aplikace právě importuje data. Počkejte, až skončí, a\u{a0}zkuste to znovu.",
        "Another window of this app is importing the data right now. Wait until it finishes, then try again.";
    ErrGameRunning => "Hra běží v\u{a0}jiném okně. Nejdřív ji ukončete a\u{a0}pak zkuste import znovu.",
        "The game is running in another window. Quit it first, then try the import again.";
    ErrIo => "Chyba při práci se souborem {path}: {reason}.", "Error accessing {path}: {reason}.";
    ErrIoOther => "Při práci se souborem {path} došlo k\u{a0}chybě.", "An error occurred while accessing {path}.";
    ErrIoDeniedHint => "Zkontrolujte oprávnění ke složce a\u{a0}zvolte Zkusit znovu.",
        "Check the folder's permissions, then choose Try again.";
    ErrFormat => "Soubor {path} je poškozený nebo není čitelný.",
        "The file {path} is damaged or cannot be read.";
    ErrIncomplete => "Obraz CD {path} je neúplný: má {actual} z\u{a0}{expected}. Kopírování nebo stahování se asi nedokončilo.",
        "The disc image {path} is incomplete: {actual} of {expected}. The copy or download was probably not finished.";
    ErrForeign => "Složka {path} nevznikla importem, proto ji hra nepřepíše. Přesuňte ji jinam nebo ji smažte a\u{a0}pak zvolte Zkusit znovu.",
        "The folder {path} was not created by an import, so it is not overwritten. Move or delete it, then choose Try again.";
    ErrCancelled => "Import byl zrušen.", "The import was cancelled.";
    IoNotFound => "soubor nebo složka neexistuje", "the file or folder does not exist";
    IoDenied => "přístup byl odepřen", "access was denied";
    IoFull => "disk je plný", "the disk is full";
    IoReadOnly => "disk je jen pro čtení", "the disk is read-only";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<&str> {
        let mut v: Vec<&str> = s
            .split('{')
            .skip(1)
            .filter_map(|p| p.split_once('}').map(|(name, _)| name))
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn strings_complete_cs_en() {
        assert!(ALL.len() > 20);
        for &s in ALL {
            let (cs, en) = (text(Lang::Cs, s), text(Lang::En, s));
            assert!(!cs.trim().is_empty() && !en.trim().is_empty(), "{s:?}");
            assert_eq!(placeholders(cs), placeholders(en), "{s:?}");
            for t in [cs, en] {
                assert!(
                    !t.chars()
                        .any(|c| matches!(c, '\u{2010}'..='\u{2015}' | '\u{2212}')),
                    "Unicode dash in {s:?}: {t}"
                );
            }
        }
        // The Czech table is Czech (diacritics present), not a copy of the English one.
        assert!(ALL.iter().any(|&s| text(Lang::Cs, s).contains('ř')));
        let same = ALL
            .iter()
            .filter(|&&s| text(Lang::Cs, s) == text(Lang::En, s))
            .count();
        assert!(same <= 2, "{same} strings are not translated");
    }

    #[test]
    fn language_from_locale() {
        assert_eq!(Lang::from_locales(["cs"]), Lang::Cs);
        assert_eq!(Lang::from_locales(["sk", "en"]), Lang::Cs);
        assert_eq!(Lang::from_locales(["CS_cz.UTF-8"]), Lang::Cs);
        assert_eq!(Lang::from_locales(["en", "cs"]), Lang::En);
        assert_eq!(Lang::from_locales(["de"]), Lang::En);
        assert_eq!(Lang::from_locales(std::iter::empty::<&str>()), Lang::En);
        assert_eq!(Lang::from_locales(["C"]), Lang::En);
    }

    #[test]
    fn fill_replaces_placeholders() {
        assert_eq!(
            fill("{a} z {b}, {a}", &[("a", "1"), ("b", "Žluť {b}")]),
            "1 z Žluť {b}, 1"
        );
    }
}
