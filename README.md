# Bootleg Toolbox

A desktop tool for collectors of lossless concert recordings: checksums, decode tests,
torrents for the trading trackers, format conversions and SBE fixing, in one program.
A modern, open-source take on Trader's Little Helper.

**One executable for Windows and Linux. Nothing else to install:** no flac, shntool,
ffmpeg or LAME. Every decoder and encoder is built in.

## Features

- **Reads** FLAC, WAV (integer and float), SHN (Shorten), APE (Monkey's Audio 3.95 and
  later) and WavPack (lossless).
- **File details**: format, length, CD-readiness, MD5 signature, sector boundary errors
  (SBE), for every track and every show.
- **Checksums**: verifies and creates `.ffp` (FLAC fingerprints), `.st5` (MD5 of the
  decoded audio, as shntool) and `.md5` (whole files). Reads files written on Windows
  (`\` paths, other letter case, Windows-1252 names).
- **Decode test**: decodes every file in full and reports damaged or truncated ones.
- **Torrents** for DIME and The Traders' Den (private, one `source` per tracker so both
  can be seeded), and for any other tracker you add.
- **Conversions**: to FLAC (levels 0 to 8, each file decoded again and checked), to WAV,
  and MP3 copies (320 kbps CBR or VBR V0) in a separate folder, with tags, text files and
  artwork. After a conversion to FLAC, a window offers to move the originals and the old
  checksum files to the trash and to create the new FFP.
- **SBE fix**, disc by disc: the same audio as `shntool fix`, written as FLAC copies in an
  `SBE-fixed` folder.
- **Lossless?**: a spectral analysis that spots lossy sources (MP3/AAC transcodes),
  upsampled audio, 16-bit audio in 24-bit files and FM broadcasts, with a full report of
  the measurements and the reasoning.
- **Speed?**: estimates whether a tape runs too fast or too slow (FM pilot tone, tuning
  against A440, mains hum), with the correction to apply.
- **Spectrograms**, to copy or save as PNG.

The original files are never changed: conversions, MP3 copies and SBE fixes are written
next to them.

## Download

Ready-to-run executables for Windows and Linux are on the Releases page of this
repository. On Windows, run `bootleg-toolbox.exe`; on Linux, make the file executable
(`chmod +x bootleg-toolbox`) and run it. To add it, with its icon, to your desktop's applications
menu, use **Help > Add to the applications menu** (again if you move the file).

## Usage

Drag folders or files onto the window (hold Ctrl to add to the list instead of replacing
it), or use **Add**. A show is a folder; its `CD1`, `CD2`, `Disc 2`, `Set 1`... sub-folders
are its discs. Select shows or tracks (click, Ctrl/Shift+click, or drag over the rows);
with no selection, actions apply to the whole list. Every action is in the toolbar, in the
**Tools** menu and on right-click.

### Command line

The same operations run without a window, for scripts:

```
bootleg-toolbox --cli <command> <folders or files...>
```

| Command | Action |
| --- | --- |
| `details` | File details |
| `verify` | Verify checksum files |
| `create` | Create checksum files (`BT_OVERWRITE=1` to replace, `BT_MD5=1` to add `.md5`) |
| `test` | Decode test |
| `torrent-dime`, `torrent-ttd`, `torrent-both` | Torrents for DIME, TTD, or both |
| `torrent-all` | Torrents for DIME, TTD and the trackers in `BT_TRACKERS="Name=announce;..."` |
| `lossless`, `speed` | Lossless and speed analyses (`BT_REPORT=1` for the full report) |
| `spectrogram` | Spectrograms, saved as PNG in the current folder |
| `to-flac`, `to-wav` | Conversions (`BT_LEVEL=0`...`8` for FLAC, 8 by default) |
| `mp3-320`, `mp3-v0` | MP3 copies (`BT_MP3_DIR` for another folder) |
| `fix-sbe` | SBE fix |

## Building from source

With a stable Rust toolchain (https://rustup.rs) and a C compiler (for LAME):

```
cargo build --release
```

The program is `target/release/bootleg-toolbox`. On Linux, building LAME also needs
`make`; no other development package is required (the X11, Wayland and OpenGL libraries
are loaded at run time).

To build the Windows executable from Linux, with the MinGW-w64 cross compiler installed:

```
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## Licence

Copyright (C) 2026 Rattlehead80

This program is free software: you can redistribute it and/or modify it under the terms
of the GNU General Public License as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version. See [LICENSE](LICENSE).

It includes LAME (LGPL) and other open-source components: see
[THIRD-PARTY.md](THIRD-PARTY.md).

Trader's Little Helper is a separate, closed-source freeware program; Bootleg Toolbox
shares no code with it.
