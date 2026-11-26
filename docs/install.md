# Instalace a hraní / Installing and playing

- [Česky](#česky)
- [English](#english)

---

## Česky

Neoficiální nativní verze hry "Kdo chce bít milionáře" (TopCD) pro současné počítače.
Instalační balíček neobsahuje žádná data původní hry. Potřebujete vlastní originální CD
nebo jeho obraz; hra si z něj data jednou zkopíruje.

### Požadavky

- **macOS:** Mac s čipem Apple (M1 a novější), macOS 11 Big Sur nebo novější.
  Macy s procesorem Intel nejsou podporované.
- **Windows:** Windows 10 nebo 11, 64bitové (x64).
- Vlastní originální CD hry "Kdo chce bít milionáře" (vydání TopCD), jeho obraz
  (soubor `.iso`), soubor `Setup.exe` z CD nebo složka, kam jste hru dříve nainstalovali.
- Asi 600 MB volného místa pro data hry.
- Volitelně pro vylepšenou grafiku: grafická karta (na Macu stačí vestavěná, ve Windows
  karta s podporou Vulkan a aktuálním ovladačem), asi 1,1 GB pro hotovou grafiku a asi
  3,5 GB volného místa během jejího vytváření.

### Instalace na macOS

1. Otevřete soubor `Milionar-<verze>-macos-arm64.dmg`.
2. Přetáhněte `Milionar` do složky `Applications` (Aplikace).
3. Aplikace není podepsaná certifikátem Apple ani notarizovaná, proto ji macOS při
   prvním spuštění zablokuje. Povolíte ji jedním z těchto způsobů:
   - Ve Finderu na aplikaci klikněte pravým tlačítkem (nebo se stisknutou klávesou
     Control), zvolte **Otevřít** a v dialogu znovu **Otevřít**.
   - Na macOS 15 a novějším tato volba nemusí stačit: zkuste aplikaci spustit, potom
     otevřete **Nastavení systému > Soukromí a zabezpečení**, sjeďte dolů a u hlášení
     o aplikaci Milionar klikněte na **Přesto otevřít**.
   - Nebo v Terminálu odstraňte příznak karantény:

         xattr -d com.apple.quarantine /Applications/Milionar.app

Kontrolní součet SHA-256 balíčku je v souboru `.dmg.sha256` vedle něj
(`shasum -a 256 -c Milionar-<verze>-macos-arm64.dmg.sha256`).

### Instalace ve Windows

1. Spusťte `Milionar-<verze>-windows-x64-setup.exe`.
2. Instalační program není digitálně podepsaný, proto se může objevit modré okno
   **Systém Windows ochránil váš počítač** (SmartScreen). Klikněte na **Další informace**
   a potom na **Přesto spustit**.
3. Projděte průvodce. Ve výchozím nastavení se hra instaluje jen pro vás, bez práv
   správce, do `%LOCALAPPDATA%\Programs\Milionar`; první dialog nabízí i instalaci pro
   všechny uživatele. Zástupce na ploše je volitelný.

Kontrolní součet je v souboru `.exe.sha256` (v PowerShellu
`Get-FileHash Milionar-<verze>-windows-x64-setup.exe`).

Nástroj pro vylepšenou grafiku potřebuje knihovnu `vcomp140.dll` (OpenMP od Microsoftu).
Instalační program ji dává přímo vedle nástroje do `upscaler\`, nic dalšího
(Visual C++ Redistributable) instalovat nemusíte.

### První spuštění: import dat z CD

Při prvním spuštění se otevře průvodce:

1. Vyberte zdroj: vložené CD, obraz CD (`.iso`), `Setup.exe` z CD nebo složku
   s nainstalovanou hrou. Soubor nebo složku můžete také přetáhnout do okna.
2. Hra zkontroluje každý soubor proti vydání TopCD a zkopíruje data (asi 350 MB).
   Import můžete zrušit; dosavadní data přitom zůstanou beze změny.
3. Potom už CD nebudete potřebovat.

### Vylepšená grafika

Po importu hra nabídne vytvoření vylepšené grafiky: všechny obrázky a videa se
jednou zvětší na trojnásobné rozlišení modelem umělé inteligence (Real-ESRGAN)
přímo ve vašem počítači. Nic se nestahuje.

- Na Macu s čipem Apple to trvá asi 8 minut, jinde může déle. Hotová grafika zabere
  asi 1,1 GB.
- Můžete ji vytvořit i později v nastavení (F10).
- Vytváření můžete kdykoli zrušit. Hotové snímky zůstanou uložené ve složce
  `upscaled/cache/` v datové složce hry (viz níže) a příští spuštění na ně naváže.
  Po úspěšném dokončení se tato mezipaměť sama smaže. Pokud vytváření dokončit
  nechcete a potřebujete místo, ukončete hru a složku `upscaled/cache` smažte ručně.
- Bez grafické karty (jen se softwarovou náhradou na procesoru) by vytváření trvalo
  několik hodin; hra se proto nejdřív zeptá.

### Ovládání

- **F8:** přepíná mezi původní a vylepšenou grafikou (pokud je vytvořená).
- **F10:** nastavení (hra se mezitím pozastaví): grafika, zobrazení (přizpůsobit, celé
  násobky, roztáhnout), celá obrazovka, hlasitost, nový import dat a nové vytvoření
  vylepšené grafiky.
- **F11** nebo **Alt+Enter** (na Macu také **Control+Command+F**): celá obrazovka.
- Ostatní klávesy a myš fungují jako v původní hře.

### Kde jsou data a záznamy

Datová složka:

- macOS: `~/Library/Application Support/milionar-port/Milionar`
- Windows: `%APPDATA%\milionar-port\Milionar`

Obsahuje `game/` (importovaná data hry), `upscaled/` (vylepšená grafika
`frames.upk`, případně rozpracovaná `cache/`), `settings.json` (nastavení)
a záznam `milionar.log` (při velikosti nad 1 MB se přesune do `milionar.log.old`).
Pokud něco nefunguje, hledejte příčinu nejdřív v `milionar.log`.

Ve Finderu se do složky dostanete přes **Přejít > Přejít do složky** (Shift+Command+G)
a cestu výše; ve Windows napište `%APPDATA%\milionar-port` do adresního řádku Průzkumníka.

### Odinstalace

- **macOS:** přesuňte `Milionar` z Aplikací do koše. Data hry zůstanou v datové složce;
  pokud je už nechcete, smažte složku `~/Library/Application Support/milionar-port`.
- **Windows:** **Nastavení > Aplikace > Nainstalované aplikace > Milionar > Odinstalovat**.
  Odinstalační program se zeptá, zda ponechat importovaná data a vylepšenou grafiku.
  Po instalaci pro všechny uživatele (a při tiché odinstalaci) data nemaže; smažte pak
  `%APPDATA%\milionar-port` ručně.

### Řešení problémů

- **"Pro vytváření grafiky je potřeba grafická karta s podporou Vulkan":** aktualizujte
  ovladač grafické karty z webu jejího výrobce (NVIDIA, AMD, Intel). Hra mezitím běží
  s původní grafikou.
- **"Nástroj pro vytváření grafiky chybí nebo je poškozený":** nainstalujte hru znovu.
  Ve Windows mohl antivirus odstranit `upscaler\realesrgan-ncnn-vulkan.exe` nebo
  `vcomp140.dll`.
- **macOS hlásí, že aplikace je poškozená nebo ji nelze ověřit:** viz krok 3 instalace
  na macOS (příkaz `xattr`).
- **Windows nespustí instalační program:** viz SmartScreen výše (**Další informace >
  Přesto spustit**).
- **Import hlásí, že soubor neodpovídá vydání:** podporované je jen vydání TopCD. CD může
  být poškozené; zkuste z něj vytvořit obraz nebo použít jinou mechaniku.
- **"Probíhá jiný import" nebo "Hra běží v jiném okně":** data nebo grafiku právě
  zpracovává jiné okno hry. Počkejte, až skončí (nebo ho zavřete), a zvolte
  **Zkusit znovu**.

---

## English

An unofficial native version of the Czech game "Kdo chce bít milionáře" (TopCD release)
for current computers. The package contains no data of the original game. You need your
own original CD or an image of it; the game copies its data from it once.

### Requirements

- **macOS:** a Mac with Apple silicon (M1 or later), macOS 11 Big Sur or later. Intel
  Macs are not supported.
- **Windows:** Windows 10 or 11, 64-bit (x64).
- Your own original CD of "Kdo chce bít milionáře" (TopCD release), a disc image of it
  (`.iso` file), the `Setup.exe` from the CD, or a folder where you installed the game
  before.
- About 600 MB of free disk space for the game data.
- Optional, for the enhanced graphics: a graphics card (on a Mac the built-in one is
  enough; on Windows one with Vulkan support and a current driver), about 1.1 GB for the
  finished graphics and about 3.5 GB of free space while they are made.

### Installing on macOS

1. Open `Milionar-<version>-macos-arm64.dmg`.
2. Drag `Milionar` onto the `Applications` folder.
3. The app is not signed with an Apple certificate or notarised, so macOS blocks it the
   first time. Allow it in one of these ways:
   - In Finder, right-click (or Control-click) the app, choose **Open**, then **Open**
     again in the dialog.
   - On macOS 15 and later that may not be enough: try to start the app, then open
     **System Settings > Privacy & Security**, scroll down and click **Open Anyway** next
     to the message about Milionar.
   - Or remove the quarantine flag in Terminal:

         xattr -d com.apple.quarantine /Applications/Milionar.app

The SHA-256 checksum is in the `.dmg.sha256` file next to the image
(`shasum -a 256 -c Milionar-<version>-macos-arm64.dmg.sha256`).

### Installing on Windows

1. Run `Milionar-<version>-windows-x64-setup.exe`.
2. The installer has no digital signature, so a blue **Windows protected your PC**
   window (SmartScreen) may appear. Click **More info**, then **Run anyway**.
3. Follow the wizard. By default the game is installed for you only, without
   administrator rights, into `%LOCALAPPDATA%\Programs\Milionar`; the first dialog also
   offers an install for all users. The desktop shortcut is optional.

The checksum is in the `.exe.sha256` file (in PowerShell:
`Get-FileHash Milionar-<version>-windows-x64-setup.exe`).

The enhanced graphics tool needs `vcomp140.dll` (Microsoft's OpenMP runtime). The
installer puts it right next to the tool in `upscaler\`; you do not need to install the
Visual C++ Redistributable.

### First start: importing the data from the CD

The first start opens a setup wizard:

1. Choose the source: the inserted CD, a disc image (`.iso`), the `Setup.exe` from the
   CD, or a folder with the installed game. You can also drag the file or folder onto
   the window.
2. The game checks every file against the TopCD release and copies the data (about
   350 MB). You can cancel the import; existing data is left unchanged.
3. After that you no longer need the CD.

### Enhanced graphics

After the import the game offers to make the enhanced graphics: every picture and video
is upscaled once to three times the resolution by an AI model (Real-ESRGAN), on your
computer. Nothing is downloaded.

- It takes about 8 minutes on a Mac with Apple silicon and may take longer elsewhere. The
  finished graphics take about 1.1 GB.
- You can also make them later in the options (F10).
- You can cancel at any time. The finished frames stay in `upscaled/cache/` in the game's
  data folder (see below), and the next run continues from them. The cache is deleted
  automatically when a run completes. If you do not want to finish and need the space,
  quit the game and delete the `upscaled/cache` folder by hand.
- Without a graphics card (only a software stand-in running on the processor) it would
  take several hours, so the game asks first.

### Controls

- **F8:** switches between the original and the enhanced graphics (once they exist).
- **F10:** options over the paused game: graphics, scaling (fit, whole multiples,
  stretch), fullscreen, volume, importing the data again and making the enhanced
  graphics again.
- **F11** or **Alt+Enter** (on a Mac also **Control+Command+F**): fullscreen.
- All other keys and the mouse work as in the original game.

### Where data and logs live

Data folder:

- macOS: `~/Library/Application Support/milionar-port/Milionar`
- Windows: `%APPDATA%\milionar-port\Milionar`

It holds `game/` (the imported game data), `upscaled/` (the enhanced graphics
`frames.upk` and, after a cancelled run, `cache/`), `settings.json` (the options) and the
log `milionar.log` (moved to `milionar.log.old` once it grows past 1 MB). When something
goes wrong, look in `milionar.log` first.

In Finder use **Go > Go to Folder** (Shift+Command+G) with the path above; on Windows type
`%APPDATA%\milionar-port` into the File Explorer address bar.

### Uninstalling

- **macOS:** move `Milionar` from Applications to the Bin. The game data stays in the data
  folder; delete `~/Library/Application Support/milionar-port` if you no longer want it.
- **Windows:** **Settings > Apps > Installed apps > Milionar > Uninstall**. The
  uninstaller asks whether to keep the imported data and the enhanced graphics. After an
  install for all users (and on a silent uninstall) it keeps them; delete
  `%APPDATA%\milionar-port` by hand then.

### Troubleshooting

- **"Making the graphics needs a graphics card with Vulkan support":** update the
  graphics driver from the card maker's website (NVIDIA, AMD, Intel). The game keeps
  running with the original graphics meanwhile.
- **"The tool that makes the graphics is missing or damaged":** reinstall the game. On
  Windows an antivirus may have removed `upscaler\realesrgan-ncnn-vulkan.exe` or
  `vcomp140.dll`.
- **macOS says the app is damaged or cannot be verified:** see step 3 of installing on
  macOS (the `xattr` command).
- **Windows does not run the installer:** see SmartScreen above (**More info > Run
  anyway**).
- **The import says a file does not match the release:** only the TopCD release is
  supported. The CD may be damaged; try making an image of it or using another drive.
- **"Another import is running" or "The game is running in another window":** another
  window of the game is working with the data or the graphics. Wait until it finishes
  (or close it), then choose **Try again**.
