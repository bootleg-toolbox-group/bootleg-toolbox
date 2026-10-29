# Bootleg Toolbox — Overview

October 25, 2026 · Rattlehead80

## What Bootleg Toolbox is

Bootleg Toolbox is a free, open-source desktop tool that does everything a collector of lossless concert recordings needs before sharing a show: check it, fingerprint it, fix it, convert it and make its torrents. It is a modern take on Trader's Little Helper (TLH), the Windows freeware the trading community has relied on for two decades.

It is built for the people who trade on Dime a Dozen (DIME), The Traders' Den (TTD) and similar trackers, and for anyone who keeps a lossless live collection in good order.

The whole program is **one executable, for Windows and Linux, with nothing else to install**. Every decoder and encoder is built in: no flac.exe, shntool, ffmpeg, LAME or Monkey's Audio to download and keep in the right folder. You download one file and run it.

Three principles guide it:

- **Your audio is never changed.** Conversions, MP3 copies and SBE fixes are written beside the files; tagging from the info file changes only tags and names, after you have checked them; every FLAC is decoded again and checked before it is kept, and nothing goes to the trash unless you ask for it.
- **Say what was found, not just "OK".** Every result tells you what was actually checked: "CD-ready, FFP-ready", "SBE on last track only", "float rounded to 24-bit, largest difference -144.5 dBFS".
- **Fast on today's computers.** Work runs on all processor cores, and the slow formats (Monkey's Audio) are decoded in parallel.

## At a glance

Sixteen groups of tasks, all in one window and all from the same right-click menu.

| Task | What it does |
| --- | --- |
| File details | Format, length, CD-readiness, MD5 signature, sector boundary errors, per track and per show |
| Verify checksums | Checks .ffp, .st5, .md5 and .sfv files, including ones written on Windows with other path styles or letter case |
| Create checksums | Writes .ffp, .st5, .md5 and .sfv files, identical byte for byte to flac/metaflac and shntool output |
| Decode test | Decodes every file in full and reports damaged or truncated ones |
| Torrents | Private torrents for DIME, TTD and any tracker you add, one per site so you can seed them all |
| Torrent check | Checks a downloaded show against its .torrent, piece by piece, and names the damaged files |
| Convert to FLAC / WAV | Levels 0 to 8, every file decoded again and verified; tidy-up window afterwards |
| MP3 copies | 320 kbps CBR or VBR V0 with LAME, in a separate folder with tags, text files and artwork |
| SBE fix | Moves track boundaries to whole CD sectors, disc by disc, with the same audio as shntool fix |
| WAV clean-up | Rewrites WAV files with a clean, canonical header, audio untouched |
| Lossless? | Spectral analysis that flags MP3/AAC transcodes, upsampling, fake 24-bit and FM broadcasts |
| Speed? | Estimates whether a tape runs fast or slow, with the correction to apply |
| Spectrograms | Native spectrograms to copy or save as PNG |
| Tag from info file | Reads the titles, artist, date and place from the info file, lets you check them, then tags and renames the FLAC files |
| Clipboard helpers | Copy a show's FFP or its info file straight into an upload form |
| Media info | A MediaInfo-style technical summary of a show's files or of a DVD, as text to copy |

## Supported formats

All five lossless formats found in trading circles are read natively, as are ALAC, MP3, MP2 and AAC, and the M4A and MKA containers; FLAC, WAV and MP3 are written.

| Format | Read and check | Write | Notes |
| --- | --- | --- | --- |
| FLAC | Yes | Yes, levels 0 to 8 | MD5 signature checked against the decoded audio; tags kept on conversion |
| WAV | Yes, 8 to 32-bit integer and 32/64-bit float | Yes | WAVE_FORMAT_EXTENSIBLE written above 16-bit or 2 channels, as the reference tools do |
| SHN (Shorten) | Yes, versions 0 to 3 | No | Decoder written for Bootleg Toolbox; length counted by decoding, since SHN does not store it |
| APE (Monkey's Audio) | Yes, version 3.95 and later | No | Decoded in parallel; APEv2 tags carried over to FLAC |
| WavPack | Yes, lossless, 16 to 32-bit and float | No | Hybrid (lossy), DSD and multichannel files are refused with a clear message |
| MP3, MP2 | Yes, gapless | MP3 only, 320 CBR and V0 | Always flagged as lossy; never converted to FLAC ("fake lossless") |
| M4A (ALAC or AAC) | Yes, ALAC 16 to 32-bit, AAC-LC | No | ALAC converts to FLAC bit for bit; AAC is flagged as lossy; iTunes tags carried over |
| MKA (Matroska audio) | Yes, with FLAC, PCM, AAC, MP2, MP3 or Vorbis inside | No | AC-3, DTS and Opus tracks are not decoded yet: the file is named with its codec; tags carried over |

Older Monkey's Audio files (before 3.95, from the late 1990s), hybrid WavPack files and AC-3 audio in MKA files are not read yet; when one turns up, Bootleg Toolbox says exactly why it cannot read it.

## Features in detail

Every feature works on a whole show (a folder with its CD1, CD2, Disc 2 or Set 1 sub-folders), on several shows at once, or on the tracks you select.

### File details

Details are read from the file headers as soon as a show is added, so the list fills in at once.

- **What is shown:** format, bits, sample rate, channels, length (in CD frames for CD audio), and a plain verdict such as "CD-ready, FFP-ready", "24-bit/96 kHz hi-res (not for CD)" or "32-bit float/44.1 kHz WAV".
- **CD-ready** means 16-bit/44.1 kHz stereo *and* no gap risk when burned. **FFP-ready** means the FLAC carries its MD5 signature.
- **Problems are named:** "Gap risk if burned: SBE before the last track", "No MD5 in header: FFP will be zeros, re-encode", "Empty or truncated: no audio samples".
- **SBE column:** "Yes" in orange for a track that needs fixing, "Last" in neutral for the normal SBE on the last track of each disc, and the show row counts only the tracks to fix.
- Details read headers only; they do not vouch for the audio itself. That is the job of Verify and Decode test.

### Verify checksums

Verify finds every checksum file in a show and checks each line against the files on disk.

- **.ffp** (FLAC fingerprints): compared with the MD5 signature in each FLAC header.
- **.st5** (shntool MD5 of the decoded audio): the audio is decoded and hashed, so it works for any format.
- **.md5** (MD5 of the whole file).
- **.sfv** (CRC32 of the whole file), with comment lines and Windows paths understood.
- Files written on Windows are understood: backslash paths, other letter case, "./" prefixes, Windows-1252 accents in names, and files moved into another sub-folder.
- The result says how many lines matched, which failed, and which files are missing or not listed.

### Create checksums

Create writes one checksum file per show, named after the show folder ("<show>.ffp").

- Choose which types to write in Settings: .ffp and .st5 by default, .md5 and .sfv on request.
- The output is identical byte for byte to metaflac and shntool: the same line format, the same order, Windows line endings by default on Windows.
- Existing files are kept unless you allow overwriting.

### Decode test

Decode test decodes every file in full, in every supported format.

- It catches damaged frames, CRC errors and truncated files, including a WAV or SHN that stops before the length its header announces.
- A FLAC's decoded audio is checked against its own MD5 signature.

### Torrents

One click makes a private torrent per tracker, ready to upload.

- **DIME and TTD built in**, with their public announce URLs; add any other tracker in Settings with its name and announce URL.
- **One torrent per site**, each with its own `source` field, so the info hashes differ and you can seed the same folder on every tracker.
- **Piece size chosen automatically** (about 1,500 pieces at most, 32 KiB to 16 MiB).
- **Junk left out:** hidden files (.directory from Dolphin, .DS_Store), Thumbs.db, desktop.ini, .part files, other .torrent files and the SBE-fixed folder.
- The torrent is saved next to the show folder (or in a folder you choose), never inside it, with a reminder to download the torrent again from the site to get your passkey.

### Torrent check

Torrent check answers the question every downloader has: is this show complete and undamaged?

- Drop a .torrent on the window, or pick it from the Check menu. The show is found by name in the list, next to the .torrent, or taken from the selection.
- Every piece is hashed again (in parallel) and compared with the torrent. Damaged pieces are traced back to the files they cover, so the result names the bad files, the missing ones and those of the wrong size.
- Files on disk that the torrent does not list are reported too, and file names that only differ in letter case are accepted.
- The log shows the torrent's name, size, piece count, private flag, source and tracker host. The passkey in the announce URL is never displayed.

### Conversions to FLAC and WAV

Conversions keep the originals and verify every result before it is kept.

- **To FLAC**, levels 0 to 8 (the level is remembered). Each file is written to a temporary ".part" file, decoded again and compared with the source audio (MD5 and sample count), and only then renamed. The audio is identical bit for bit (checked after every file), and so is the MD5 signature stored in the FLAC, so FFP files match those of any other encoder; only the compression differs slightly, so files come within 0.3% of the size the reference flac encoder would give.
- **Tags carried over:** FLAC Vorbis comments, APEv2 tags from APE and WavPack files (year, track and disc numbers, album artist), iTunes tags from M4A files and Matroska tags from MKA files.
- **To WAV** from any format, also verified.
- **After a conversion to FLAC**, a window offers to move the converted originals to the trash, to remove the old .md5/.st5 files that list them, and to create the new FFP. Your choices are remembered.

### 32-bit float WAV to FLAC

FLAC stores whole numbers only, so float WAVs need care: many tools refuse them, others convert them silently.

- Bootleg Toolbox reads the float files first. When the float only holds 16-bit or 24-bit values (common when an editor exported a recording as float), it converts them **bit-exact**.
- Real float audio is rounded to 24-bit only after you agree. A window shows each file's peak and any samples above 0 dBFS that would be clipped.
- The largest difference is measured and reported, for example -144.5 dBFS, far below any audible level. The originals of rounded files are kept and not offered for the trash.

### MP3 copies

MP3 copies are for listening on a phone or in the car, never for trading, so they go in their own folder.

- **320 kbps CBR or VBR V0**, encoded with LAME 3.100 at high quality, gapless (exact sample count, LAME/Xing header written).
- Written to "<show> [MP3 320]" or "<show> [MP3 V0]" next to the show, or in a folder chosen in Settings, keeping the CD1/CD2 sub-folders.
- **Tags** become an ID3v2.3 tag (title, artist, album artist, album, year, track n/total, disc n/total, genre, comment) in UTF-16, so accents and non-Latin scripts show correctly in Windows Explorer.
- Hi-res sources are resampled to 48 or 44.1 kHz. Text files, artwork and PDFs are copied beside the MP3s.
- The log gives a clear tally per show: encoded, skipped because already there, failed.

### SBE fix

The SBE fix makes every track of a disc end on a whole CD sector, so it burns without gaps or clicks.

- Disc by disc (each CD1, CD2... folder), every boundary moves back to the previous sector (588 samples). The audio past it moves to the start of the next track, and the last track is padded with silence.
- **The audio is identical to `shntool fix`**, checked track by track.
- Corrected copies are written as verified FLAC in an "SBE-fixed" folder, with tags kept; the originals are untouched. Tracks already aligned before the first error are not rewritten.
- Tracks are written in parallel, since each one only depends on the track lengths.
- A disc with a track shorter than one sector is refused rather than producing an empty file, as shntool would.

### WAV header clean-up

Some recorders and editors write WAV files that older tools choke on. Clean WAV headers fixes them without touching a single sample.

- **Detected:** extra chunks (LIST, bext, junk), WAVE_FORMAT_EXTENSIBLE used for plain 16-bit stereo, an oversized fmt chunk, a wrong RIFF size, a size left unfilled by a recorder that stopped early, stray bytes after the audio. A truncated file is reported as an error and left alone.
- **Clean copies** go to a "WAV-cleaned" folder with a canonical 44-byte header (or the extensible header where it is required). Each copy is checked: its audio MD5 must match the original.
- Float WAVs are left as they are: their header is already correct, and they have their own conversion path.

### Tag and rename from the info file

Most shows arrive with a text file describing them; Bootleg Toolbox turns it into tags and file names, after you have checked them.

- **What is read:** the artist, date and place from the top of the file or from "Artist:", "Date:", "Venue:" fields; the tracklist, numbered ("01 - Title", "1. Title", "[01] Title", "d1t01 Title") or plain, across "Disc 1", "CD 2:" or "Set 2" headings, with running times, footnote marks and fingerprint lines left out. Several tracks on one line are told apart ("01 Intro, 02 Nightrain", "1. Intro 2. It's So Easy", "1.INTRO 2.IT'S SO EASY") when their numbers follow on.
- **Titles from the file names:** when the info file has no tracklist but every file name holds a title ("01 - It's So Easy.flac"), those titles are used and the show is marked to check; a button does the same in any show. A "Title Case" button turns titles written in capitals into "It's So Easy", keeping initials (N.I.B.), Roman numerals and AC/DC. Several text files: the one whose tracklist matches the number of files comes first.
- **A window to check everything** before it is written: artist, date, place, album (the eTree "date place" by default), each title next to its file and its new name. It warns when the list and the files do not have the same count, when the folder name gives another date, and when a new name would clash.
- **Written:** TITLE, ARTIST, ALBUM, DATE, TRACKNUMBER, TRACKTOTAL, and DISCNUMBER/DISCTOTAL for several discs; the other tags are kept. Names: "01 - Title", "d1t01 - Title" or "Artist YYYY-MM-DD d1t01".
- **Tags, names or both:** "Write the tags" and "Rename the files" are two boxes, ticked by default in Settings and changeable for each show. Renaming alone does not write in the files, so it works on read-only files too. A date not written YYYY-MM-DD is pointed out.
- **Where the show is named:** when the audio sits in an "AUDIO" (or "FLAC", "24-bit"...) folder, or below a dated folder ("Artist - 1995-11-21 - Place/1995-11-21 - Madrid (Version 2)"), the artist, date, place and info file are looked for in the folder above, and the list shows both names.
- **Several shows at once:** select them and open Tag and rename. Each show's plan is prepared and listed. The clear ones are ticked: a single info file whose tracklist fits the files, as many titles as files, the artist and a full date there, agreeing with the folder name. The others say what to check ("29 titles for 4 files", "no date", "2 info files fit, with other titles"...) and open one by one in the usual window, whose OK ticks them. Apply writes the ticked shows in one go; the rest can wait. Shows without an info file are listed as such and left alone.
- **The audio is never rewritten:** only the FLAC header changes, in place when its padding has room. The audio signature is checked after every file. FFP and ST5 stay valid; .md5 and .sfv lines of the tagged files are computed again, and new names are written into every checksum file. Lines that did not match the files before tagging are reported, as tagging rewrites them. A .torrent made before is reported as out of date.
- **Nothing is changed when a file is read-only** (a torrent client may still hold it): you are told which files to unlock, so a show never ends up with names but no tags.
- **Undo:** before a show is changed, its FLAC headers, former names and checksum files are kept for 30 days in the program's data folder (not in the show's folder). Tools > Undo tagging lists each Apply with its shows; rolling back gives the files back byte for byte, so the checksums and torrents made before match again. A file changed since by another program, or whose former name is taken, is left as it is and reported, and the checksum files are then kept as they are. From the command line: `undo-tagging`.

### Lossy files (MP3, MP2, AAC)

MP3 and MP2 files, and AAC in M4A or MKA files, can be added like any other show, mostly to recognise them for what they are.

- Details, decode test, spectrogram, checksums and torrents work on them; decoding is gapless when the LAME header gives the encoder delay (AAC files include the encoder delay, which only matters for joining tracks).
- They are marked "Lossy file" and never called CD-ready. Lossless? gives a firm "Lossy (MP3)" or "Lossy (AAC)" verdict, with the measures for reference.
- To FLAC is refused, since it would only make a "fake lossless" file. To WAV is allowed, for burning or editing.

### Lossless? (lossless origin check)

Lossless? looks at the spectrum of every track to tell whether a lossless file really comes from a lossless source.

- **Verdicts:** Lossless; Lossy (an MP3 or AAC wall, with the bitrate it matches); Lossy (MP3 traces); Lossy (AAC traces); Lossy (lossyWAV); Lossless (upsampled); Lossless (16-bit in 24); FM (15 kHz) for radio broadcasts; Lossy upstream, for a lossless capture of a stream that was already compressed (a station's digital link, an internet stream).
- **Measures behind each verdict:** the cutoff frequency, the drop above it and the steepness of its edge (in dB per octave), the "encoder holes" lossy codecs leave just below their cutoff, the noise floor above the cutoff, the FM pilot tone at 19 kHz (and a shifted pilot when the tape speed is off), the TV line whistle at 15,625 or 15,734 Hz, and stereo width.
- **MP3 and AAC coding traces, even at 320 kbps:** the sound is analysed again with the encoder's own transform (MP3's filter bank, AAC's MDCT with both of its window shapes), at every possible position; a file decoded from an MP3 or an AAC falls back onto the encoder's rounding grid. It needs no cutoff, so it catches the high-bitrate files a spectrum cannot prove (iTunes, YouTube and streaming rips included), through a level change, a 24-bit decode or a trimmed start. The method comes from published research (O. Derrien, CNRS, Journal of the Audio Engineering Society, 2019); on our test banks it found every MP3 from 128 kbps to 320 kbps and V0, and every AAC from two different encoders from 128 to 320 kbps, with a wide margin over genuine files.
- **lossyWAV:** files processed by lossyWAV (which zeroes the lowest bits of each block to make smaller FLACs) are recognised by their zero bits that change from block to block.
- **Bits really used:** a 24-bit file made from 16-bit audio is caught even when its level was changed afterwards (its sample values form a comb), not only when its lowest 8 bits are zero.
- Tuned on 42 real FM shows from 1977 to 2011, and on MP3 and AAC control files.
- **See more:** a click on any verdict opens the full report: each measure with what is normal, the reasoning step by step, and the conclusion. The report can be copied as text, and the show report links to each track's.

### Speed? (tape speed check)

Speed? estimates whether a recording runs too fast or too slow, with the correction to apply.

- **Three independent clues:** the FM pilot tone (exact when present), the tuning of the music against A440, and the mains hum at 50 or 60 Hz.
- Each clue gives a percentage only when it is reliable. When the clues disagree by more than 1%, the result says so ("Unclear (conflict)") instead of guessing.
- The report states the known blind spot: a tape off by exactly one semitone (about 5.9%) cannot be told from correct tuning by ear alone.
- Checked on a 1979 show whose pilot tone was 1.1% low; the tuning analysis found 1.12%.

### Spectrograms

Spectrograms are drawn natively, with no external tool.

- 1024 x 512 pixels, -120 to 0 dBFS, with a legend (frequency, time, level).
- Shown in their own window, which can be moved outside the main window; copy to the clipboard or save as PNG next to the track.

### Clipboard helpers

Two one-click helpers fill in an upload form.

- **Copy FFP contents** copies the show's .ffp. When a show has none (only an .md5, say), the FFP is built on the spot from the FLAC headers, identical to what Create would write.
- **Copy info file** copies the show's info .txt or .nfo, whatever its name. When there are several, a window lists them with a preview, the most likely one first. Windows-encoded accents are converted.

### Media info

Media info writes a technical summary in the manner of MediaInfo, as plain text ready to paste into a forum post or an info file. Only file and folder names appear, never full paths.

- **For a show** (Ctrl+I, the Check menu or the right-click menu): file count, total length and size, average bit rate, common format ("CD audio" when it applies) and sector boundary errors, then for each file its format and compression, encoder (libFLAC version, LAME version and its "-V 0" setting for VBR MP3, Monkey's Audio version and compression level), length, bit rate, channels, rate, bits, audio MD5 and tags.
- **For a DVD**, add or drop its folder (VIDEO_TS or the folder above it), or use Add > DVD media info. DVDs do not join the list: their summary opens in its own window.
- **What a DVD summary gives:** whether it fits a DVD-5 or needs a DVD-9, the main title, and for each title set its length, overall bit rate, video (MPEG-2 profile, resolution, PAL or NTSC, frame rate, 16:9 or 4:3, interlaced or progressive), an estimate of the video bit rate, every audio track (AC-3, DTS, LPCM or MPEG, channels such as 5.1, rate, bit rate, language), subtitles, and every title with its chapters.
- **Lossless or lossy, said plainly:** each DVD audio track is marked as lossless (LPCM) or lossy (AC-3, DTS, MPEG), and an "Audio extraction" line says whether a track can be extracted to WAV or FLAC without loss.
- Loose VOB files without their IFO are read too, their length taken from the video timestamps.
- Checked against MediaInfo on test material: the same bit rates, channels and lengths.

### File manager integration

An optional "Open in Bootleg Toolbox" entry can be added to the right-click menu of folders and audio files (Settings, off by default).

- **Windows:** in Explorer, for the current user only, with no administrator rights. On Windows 11 it is under "Show more options", as for every program that is not from the Microsoft Store.
- **Linux:** Dolphin, Nautilus and Nemo, whichever are installed.
- The entry follows the program if you move it, and unticking the option removes it completely.

### Update check

The program can tell you when a new version is out, but only if you want it to.

- **Help > Check for updates** asks once; the **Settings option** "Check for a new version at startup" asks at each start. It is off by default.
- When a newer version exists, the log says so and the Help menu offers "Download version X", which opens the release page.
- One request goes to github.com, sending nothing but the program's name and version. The connection is encrypted, and certificates are checked by the operating system (the Windows certificate store, or the distribution's certificates on Linux).

### Command line

Every operation also runs without a window, for scripts and batch work: `bootleg-toolbox --cli <command> <folders or files>`.

- Commands: details, verify, create, test, torrent-dime, torrent-ttd, torrent-both, torrent-all, torrent-check, lossless, speed, spectrogram, to-flac, to-wav, mp3-320, mp3-v0, fix-sbe, clean-wav, media-info, check-update, shell-menu-add, shell-menu-remove, help.
- Options through environment variables (`BT_...`): overwrite, extra .md5 and .sfv, FLAC level, extra trackers, MP3 folder, full reports, float rounding.
- `bootleg-toolbox --help` (or `--cli help`) prints every command, variable and keyboard shortcut, with examples for Linux, the Windows Command Prompt and PowerShell. Exit codes: 0 when everything went well, 1 when something failed, 2 for a wrong command.

## Usability

The interface is one compact window, close in spirit to TLH: a list of shows and tracks, a toolbar of five menus, and a log you open only when you need it.

### Getting shows in

- **Drag and drop** folders or files from Explorer, Dolphin or any file manager. A drop replaces the list; hold Ctrl to add to it instead. Dropped .torrent files are checked against their show, and dropped DVD folders open their media info.
- **Right-click a folder in the file manager** and choose "Open in Bootleg Toolbox", once the option is on.
- **Add folder / Add files** dialogs, which add without duplicates.
- **Shows are recognised automatically:** a folder is a show, and its disc sub-folders belong to it: CD1, CD2, Disc 2, D1, Set 1, Set B, Side A, Part 2, Tape 1, Vol 1, or a bare letter or number (A, B, 1, 2). Tracks are sorted naturally (track 2 before track 10). Copies (MP3, SBE fix) keep the same sub-folders.

### Selecting

- Click anywhere on a row; Ctrl+click and Shift+click to add or extend; drag over rows (or from the empty area below them) to select a range, with automatic scrolling near the edges; Ctrl+A to select everything.
- Escape or a click in an empty area clears the selection.
- **With nothing selected, actions apply to the whole list**, and the status bar says so in orange, so it never happens by surprise.

### Menus and shortcuts

- **Toolbar on one line:** Add, Check, Create, Convert, Torrent, with Stop and Settings on the right.
- **Right-click** on any row gives every action, with Open folder first. For a track, the folder opens with the file selected (Explorer on Windows, Dolphin, Nautilus and others on Linux).
- **Keyboard:** Page Up/Page Down and Home/End move through the list; F1 help, F5 verify, F6 create, F7 decode test, F8 details, F9 spectrogram, F10 torrents for all trackers, Ctrl+L Lossless?, Ctrl+P Speed?, Ctrl+I media info, Ctrl+A select all, Delete removes from the list, Escape stops a running task (or clears the selection, or closes a window).
- **Help > Keyboard shortcuts and command line** (F1) lists every shortcut, mouse gesture and command-line option in one page, with a Copy button.
- Menus scroll instead of spilling over the window when it is small.

### The table

- Columns: Name, Format, Bits, Rate, Length, Ch, SBE, Source, Speed, Result. Any of them can be hidden (View menu or right-click on the header), and widths are remembered.
- Columns fit their content after each operation; double-click a separator to fit one by hand.
- **Compact view** hides Bits, Rate and Ch (Format then reads "FLAC 16/44.1") and tightens the rows.
- **Colours carry meaning:** green for OK, orange for a warning, red for an error. Every result has a tooltip with the details.

### Feedback while it works

- A progress bar with a count, and the running operation shown in bold purple with a spinner, so a running task cannot be missed.
- Stop (or Escape) cancels cleanly: no half-written file is left behind.
- **The log** folds to one line showing the last message, unfolds by itself on an error, and can be cleared or saved to a file.

### Settings and comfort

- Light, dark or follow-the-system theme (read from the desktop on Linux as well).
- **Always on top**, remembered between sessions.
- Settings cover checksum types, overwrite, line endings, number of parallel jobs, tracker announce URLs and extra trackers, torrent and MP3 folders, the file manager entry and the update check. All choices are remembered. The Settings window scrolls on small screens, with its buttons always in view.
- The window opens at a modest 1000 x 465 and remembers its size.
- On Linux, Help > Add to the applications menu installs a menu entry with the icon, for the current user, without administrator rights.
- All texts are in English, for the international trading community.

## Performance

Most everyday tasks finish in seconds: work is spread over all processor cores, and audio is processed as a stream, so memory use stays small even for long files.

Measured on a desktop Linux PC during development (times include reading the files):

| Task | Material | Time |
| --- | --- | --- |
| Decode test, FLAC | one 2 min 40 s track | 0.17 s |
| Decode test, APE (high compression) | the same track | 0.85 s (ffmpeg: 2.2 s) |
| Decode, SHN | 10 tracks, 338 MB | 0.7 s |
| Spectrogram, FLAC | one 2 min 40 s track | 0.7 s |
| Spectrogram, APE | the same track | 1.5 s |
| SBE fix, FLAC level 8 output | 8 tracks, 2 shows | 3.6 s |
| MP3 VBR V0 | 12 tracks | 4.2 s |
| MP3 320 CBR, including 24/96 to 48 kHz | 12 tracks | 13.7 s |

### How it gets there

- **Parallel by default:** files are processed side by side, up to 8 at once (adjustable in Settings).
- **Monkey's Audio decoded in parallel:** its frames are independent, so each track is split across cores. This made APE decoding about 5 times faster.
- **SBE fix in parallel:** each corrected track only depends on the track lengths, so all of them are written at the same time instead of one after the other.
- **No external processes:** nothing is piped through flac.exe or ffmpeg, which saves the start-up and copying costs of command-line tools.
- **Details read instantly** from the headers when a show is added, even for large collections.

### Honest limits

- FLAC encoding uses a pure-Rust encoder: the audio is the same bit for bit and the files come within 0.3% of the reference encoder's size, but encoding is 2 to 5 times slower per file than the reference flac.exe. Parallel processing makes up most of the difference on a whole show.
- The Lossless? and Speed? analyses decode every track in full, so they take about as long as a decode test plus the analysis itself.

## Platforms, installation and safety

Bootleg Toolbox runs on 64-bit Windows and Linux from a single file, with nothing to install alongside it.

### Installing

- **Windows:** download `bootleg-toolbox-windows-x86_64.exe` (about 9 MB) and run it. Until the executable is code-signed (the application to the SignPath Foundation is in progress), Windows SmartScreen shows "Windows protected your PC" on first launch: click More info, then Run anyway.
- **Linux:** download `bootleg-toolbox-linux-x86_64` (about 18 MB), make it executable and run it. It works on any distribution from 2022 onwards (glibc 2.35: Fedora 36+, Ubuntu 22.04+, Debian 12+, Mint 21+). Help > Add to the applications menu adds it to the desktop menu with its icon.
- **Everything is inside:** the decoders, the FLAC and MP3 encoders, the interface, the fonts and the icon. The program only uses the system's C library and the display libraries every desktop already has.
- Each release lists SHA-256 checksums of both files, so a download can be checked.
- Settings live in the user's profile; removing the program means deleting the file.

### Safety of your files

- **Originals are never modified** by conversions, MP3 copies, SBE fixes and WAV clean-ups: they write new files. Tag and rename changes tags and names in place, only after you have checked them, never the audio, and Tools > Undo tagging gives the files back byte for byte.
- **Nothing half-written:** new files are written as ".part" and renamed only once complete and verified; a stopped or failed job removes them.
- **Existing files are not overwritten** without your say: an existing FLAC, MP3 or checksum file is skipped and reported.
- **Nothing is deleted:** the after-conversion window moves files to the system trash (Recycle Bin, or the desktop trash on Linux), only if you tick the box, so they can be restored.
- **No network access unless you ask:** the only connection is the optional update check, off by default. Torrents are written to disk for you to upload yourself.

### Built in the open

- Written in Rust, a language designed for memory safety, which prevents whole classes of crashes on damaged files.
- Released executables are built by GitHub's servers from the published source, with the tests run on every build.

## Comparison with Trader's Little Helper

Bootleg Toolbox covers the everyday jobs TLH is used for and adds Linux, WavPack, Unicode names, deep source analysis, DVD media info and parallel speed; TLH still handles a few legacy formats and extras that Bootleg Toolbox does not. TLH's last release is 2.8.4 (October 2020), and its site states that development has been discontinued ([TLH website](https://tlh.easytree.org/)).

| | Trader's Little Helper 2.8.4 | Bootleg Toolbox |
| --- | --- | --- |
| Platforms | Windows (XP or later) | Windows and Linux |
| Distribution | Installer bundling 8 command-line tools (flac, metaflac, shntool, shorten, mac, lame, mkwcon, aucdtect) | One self-contained executable, no external tools |
| Source code and licence | Closed, freeware for non-commercial use | Open source, GPL-3.0 |
| Development | Discontinued (last release October 2020) | Active |
| Unicode file names | Not supported | Supported (accents, non-Latin scripts) |
| Read FLAC, WAV, SHN, APE | Yes | Yes |
| Read WavPack | No | Yes |
| Read MP2, MP3 | Yes | Yes, gapless, always flagged as lossy |
| Read M4A (ALAC, AAC) and MKA | No | Yes (AC-3 in MKA not yet) |
| Read MKW | Yes | No |
| 32-bit float WAV | Not described | Read; bit-exact or measured 24-bit conversion to FLAC |
| Encode FLAC | Yes, through flac.exe | Yes, built in, verified after every file |
| Encode APE, SHN | Yes | No (no longer used for trading) |
| MP3 | Yes, through lame.exe | 320 CBR and V0, gapless, in a separate folder with tags and artwork |
| Checksums | CFP, FFP, MD5, SFV, ST5 | FFP, MD5, SFV, ST5; FFP also built from FLAC headers on the fly |
| Decode test | APE, FLAC, SHN, MKW | Every read format, with truncation detection |
| SBE | Fix in APE, FLAC, SHN, MKW, WAV, MP3 | Detection per track and per disc; fix with the same audio as shntool, FLAC output |
| Lossy source check | Tests WAV files for an MP3 source (auCDtect) | Lossless? spectral analysis on every format, MP3 and AAC coding traces up to 320 kbps, lossyWAV, with FM, upsampling, fake 24-bit and "lossy upstream" verdicts and a full report |
| Tape speed check | No | Speed? (pilot tone, A440 tuning, mains hum) |
| Spectrograms | No | Yes, with PNG export |
| Torrents | Create, show contents, hash-check against local files | Create, one per tracker with its own source field; extra trackers; hash-check naming the damaged, missing and extra files |
| WAV clean-up | Removes extra RIFF chunks, rewrites canonical headers | Yes, into a separate folder, audio checked by MD5 |
| Technical summary (MediaInfo-style) | No | Audio files and DVDs (video, audio tracks with lossless/lossy, subtitles, titles, chapters) |
| SHN seek tables (SKT) | Yes | No |
| Tag from info file | No (Live Show Tagger, a separate foobar2000 component, last updated in 2010) | Yes, checked in a window, checksum files kept up to date |
| Upload helpers | Not described | Copy FFP, copy info file |
| File manager integration | Explorer | Explorer, Dolphin, Nautilus, Nemo (optional) |
| Update check | Yes | Yes, optional and off by default |
| Built-in help | Help file | Shortcuts and command line in one page (F1, --help) |
| Speed on multi-core PCs | Not described | All cores; APE decoded in parallel |
| Dark theme, always on top | Not described | Yes |
| Command line | Not described | Every operation |

### Where Bootleg Toolbox goes further

- **Linux and modern Windows**, from one file that cannot break because a bundled tool was replaced or is missing.
- **WavPack and Unicode names**, both common in collections today.
- **Source analysis** that explains itself: TLH relies on auCDtect, a closed tool that tests WAV files and gives a verdict without its reasons, while Lossless? works on every format and shows its measures and reasoning, and recognises FM broadcasts instead of calling them lossy.
- **Tape speed, spectrograms, float WAV, tagging from the info file, upload helpers and DVD media info**, none of which TLH offers.
- **Open source and maintained:** bugs can be fixed and the tool can follow new needs.

### Where TLH still does more

- Legacy formats: MKW decoding, APE and SHN encoding.
- CFP checksum files and SHN seek tables (SKT).

APE and SHN encoding, MKW and SKT were left out on purpose: these formats are no longer used for trading.

## Licence, credits and next steps

Bootleg Toolbox is free software by Rattlehead80, released under the GNU General Public License v3.0 or later: anyone may use, study, share and improve it, and improved versions stay open.

- **Built with open-source components:** egui and eframe (interface), Symphonia (FLAC, MP3, AAC and ALAC decoding, M4A and MKA files), flacenc (FLAC encoding), ape-decoder (Monkey's Audio), wavicle (WavPack), LAME 3.100 (MP3), rustls (encrypted update check) and the Noto fonts, among others. Every component is under an OSI-approved open-source licence. The full list with licences is in the About window and in THIRD-PARTY.md in the source.
- **The Shorten (SHN) decoder** was written for Bootleg Toolbox and checked bit for bit against ffmpeg.
- **Not affiliated with Trader's Little Helper:** the two programs share no code.

### What comes next

- **Code signing for Windows**, through the free SignPath programme for open-source projects, to remove the SmartScreen warning. The project's code signing policy is in its README.
- **Linux packages:** a Fedora COPR repository and a Flathub package, for installation and updates through the usual software tools.
- **Community requests**, as and when traders ask for them.

Feedback, bug reports and ideas are welcome on the project's GitHub page.

### Sources

- Trader's Little Helper feature list, version and licence: [tlh.easytree.org](https://tlh.easytree.org/) (opened October 25, 2026).
- Bootleg Toolbox figures: measured during development on the project's test files.
