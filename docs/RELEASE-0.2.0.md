# Bootleg Toolbox 0.2.0: what changed, in detail

Everything below is compared with **0.1.14**, the last published version. Figures come from the project's own test sets (named each
time) and are measured, not promised: a lossless verdict is "no sign of loss found", never a proof.

## Highlights

- **Lossless? tells proof from doubt.** A red verdict needs a proof (coding traces, or two independent clues); one clue gives orange;
  no verdict either way gives the blue **Unclear**, now used much more widely. Fewer false accusations, and fewer MiniDisc recordings
  passed as lossless, at the price of more "cannot tell".
- **It finds lossy stretches inside a file**, MP2 transcodes, and AAC transcodes in the default mode.
- **The analysis does more, and keeps your computer usable.** It uses more of the processor but at the lowest priority.
- **Stop is almost immediate** (under a second), with a clear "Stopping…" state.
- **The menus are ordered by what the actions do to your files**, and the right-click menu is shorter.

## What you will notice

### Lossless? (verdicts)
- **"Unclear" (blue) appears more often.** In 0.1.14 it meant almost no treble; now it means the evidence does not allow a verdict
  either way (little treble, a treble cut with hiss above and no FM pilot, a wall near 20 kHz with digital silence above it, a
  MiniDisc-like roll-off). It is not an accusation. Where 0.1.14 said "Lossy?" or "Lossless" on weak ground, 0.2.0 may now say "Unclear".
- **Evidence by families, with a confidence.** Every verdict carries a confidence (high, medium, low) in its report, with the clues for
  and against, step by step; a file that is lossy by its format is always high.
- **New and renamed labels.** "Lossy (MP2 traces)"; "Lossy part mm:ss–mm:ss" (red) and "Lossy? part mm:ss–mm:ss" (orange) for a stretch
  of a file that is not lossy as a whole; "Uneven passage mm:ss–mm:ss" (information only); "Part grid not counted" in the report;
  "Lossless (no hi-res content)" for a hi-res file with nothing above 22–24 kHz. The orange "Unsure 16.0 kHz" is now "Lossy? 16.0 kHz",
  and "Lossy? (Opus)" is now "Lossy (joint-stereo treble)" or its orange form.
- **Parts.** Each file is also read in 10 s slices (half overlapping). A lossy stretch is red only with a coding trace found in it,
  and a coding grid in a stretch counts only if it stands out more than chance allows given how many stretches were searched.
- **MP2 coding traces** (MPEG-1 Layer II) are found from the encoder's own filter bank.
- **AAC traces in the default mode.** Without the "look for AAC, Opus and MP2 traces" option, one track of each show, **the longest
  track other than the first and the last** (with fewer than three tracks, the longest), is searched for AAC traces; if it has them,
  the other tracks of the show are searched too. An AAC transcode that shows nothing in the spectrum is now found without the slow
  option.
- **Noise reducers.** Holes under a cut whose digital silence above does not follow a fixed lowpass (a noise reducer) stay a doubt
  instead of becoming proof.
- **Verify** keeps every result of a file (an error is never hidden by a later OK) and shows the files a checksum file lists that are
  not there, in a "(missing)" line.

### The analysis and your computer
- **Files processed in parallel: Auto** (Settings): every logical processor but one (at least 2), fewer when half the free memory would
  not hold them (about 300 MB each). A number you saved by hand is kept; a saved value equal to the old default moves to Auto.
- **Lowest priority** for the analysis threads (Linux: idle scheduling class and idle I/O class; Windows: background mode), so the
  desktop stays responsive during a long analysis, even though more of the processor is in use.
- **Analysis time, compared with 0.1.14** (1 550 files of the tuning set, a machine at rest, the two versions run alternately; 0.1.14
  with its 9 threads, 0.2.0 with Auto = 19 threads):
  - **Default mode: about 20 % longer** (774 s against 646 s, four passes against two), because it does more: the parts, the AAC
    search on a sample track of each show, the stricter decision. It uses about 2.4 times the processor time.
  - **With the slow searches (AAC, MP2, Opus; the option in Settings): about 36 % faster** (1 938 s against 3 015 s), thanks to
    faster searches (their results are bit-for-bit identical to the same searches before the speed-up, checked file by file on
    3 018 test files in both modes) and to the extra threads.
- **Stop is almost immediate.** The request is read inside the long searches: the analysis ends in under a second (0.02–0.64 s
  measured on a 93-minute concert), where it could take 15 to 20 s. "Stopping…" shows at once, in the status bar and on the button.
  A file that was stopped has no verdict; the concert's row then reads "Stopped: N of M tracks analysed", in plain text with no
  verdict colour, and the tracks already analysed keep their own verdicts.

### Menus
- **Toolbar:** Add, Check, Edit, Convert, Share (the Create and Torrent menus are gone).
  - **Check** (reads, writes nothing): Verify checksums, Decode test, Check against torrent, Lossless?, Speed?, Spectrogram, Show details, Media info.
  - **Edit** (corrects and completes a show): Tag and rename from info file, Undo tagging, Get setlist from setlist.fm; Fix SBE and
    Clean WAV headers (both write to a new folder).
  - **Convert** (makes copies, the originals are kept): To FLAC (verified, levels 0–8), To WAV, MP3 320, MP3 V0, Extract audio from video.
  - **Share** (what goes to a tracker): Create checksums, Torrent for both sites (or all trackers), one entry per site, Copy info file,
    Copy FFP contents, Share card.
- **Right-click:** Decode test, Lossless? (and "See lossless/speed analysis" when the row has one); Create checksums, Torrent for both
  sites; Copy info file, Copy FFP contents; sub-menus Check, Edit, Convert, Share; Open folder, Remove from list. What does not apply
  is left out, not greyed: on a track, the checksums, torrents and copies work on the show that holds it; a file listed as missing
  only has Open folder and Remove from list.
- The **Tools** menu lists the same four groups one after the other. Shortcuts are unchanged.

## Measured progress, compared with 0.1.14

On the judging set of 158 concerts (3 268 files) read in the default mode; each concert has a known or reported origin.

- **No red on lossless concerts, as with 0.1.14, with less doubt shown.** The 49 concerts whose lossless origin is asserted, the 7
  Hi-MD PCM ones among them, and 3 patched control concerts: 0 red, as before. Orange on the asserted ones: 6 concerts (55 tracks)
  with 0.1.14, 4 (9 tracks) with 0.2.0, and 2 more concerts (40 tracks) now "Unclear".
- **No "Lossy part" in red on lossless tracks:** 0 of 1 618 tracks, as before; 4 of them carry the orange "Lossy? part" in 0.2.0.
- **One red less.** Among 41 concerts whose lossless origin is only assumed, 0.1.14 showed one red (13 tracks "Lossy 18.7 kHz");
  0.2.0 shows none.
- **MiniDiscs are clearly better handled** (29 concerts): 16 flagged instead of 12 (by recording chain: 14 of 25 instead of 11 of 25);
  only 9 still shown "Lossless" instead of 15; 3 red instead of 10, since most of the former reds are now orange or Unclear.

## Known limits and setbacks

- **A few lossy files that 0.1.14 flagged as doubtful are now "Unclear".** Of the 21 lossy concerts that show no sign in the spectrum,
  8 are flagged (red or orange) instead of 10, among them 3 concerts that were orange and are now Unclear, and 2 that were red and
  are now orange. The number shown "Lossless" by mistake is unchanged (10 of 21). Of the 5 Hi-SP concerts (a very small sample),
  1 is flagged instead of 3.
- **Patched concerts with a lossy piece are still not detected.** A concert patched with a short lossy stretch (a 5–20 s MP3 piece, a
  whole track taken from YouTube) stays "All lossless": 0 of 4 on the judging set, in 0.1.14 and in 0.2.0. The parts only name
  stretches with their own coding trace or two independent clues.
- **MiniDisc (ATRAC) has no detector.** MiniDisc recordings stay "Unclear" (or, at worst, "Lossless") unless the spectrum shows a cut
  or holes; the gain above comes from the spectrum only.
- **Denoised sources without holes** (a noise reducer or a gate that leaves no encoder-like holes under the cut) are not detected.
- **Slow searches** stay limited: AAC and MP2 traces at 32, 44.1 and 48 kHz only, Opus traces in the first minute; AAC is not searched
  at 88.2/96 kHz; some MP3 at 320 kbps are not found.
- **A lossless verdict is not a proof.** Resampling erases the encoders' coding traces, and enough added noise can hide any lossy origin.
- **Windows 11, recent versions: the Explorer entry.** On recent versions of Windows 11 (the new right-click menu of autumn 2026), the
  "Open in Bootleg Toolbox" entry no longer appears in the new menu: "Show more options" is disabled by default, and the integration
  uses the older registry mechanism. The entry is in the classic menu: hold Shift while right-clicking, or turn "Show more options"
  back on through "Customize the menu". Integration with the new menu is planned for a later version.

## Updating

- Settings are kept. "Files processed in parallel" becomes **Auto** if it still had the old default; a number you chose stays.
- Lossless? may now show "Unclear" where 0.1.x said "Lossy?" or "Lossless": rerun it on the shows you care about.
- Menus moved: Create and Torrent are now in Share; Fix SBE, Clean WAV headers, Tag and rename and Undo tagging are in Edit.
  Keyboard shortcuts did not change.
