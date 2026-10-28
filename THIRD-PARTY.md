# Third-party software

Bootleg Toolbox is free software under the GNU General Public License v3.0 or later
(see [LICENSE](LICENSE)). The executable also contains the following software, under
licences compatible with the GPL.

## LAME (MP3 encoder)

LAME 3.100 (https://lame.sourceforge.io/), written in C, is compiled into the executable
through the `mp3lame-sys` and `mp3lame-encoder` crates. LAME is distributed under the
GNU Library General Public License (LGPL), version 2 or later; its full source code is the
`lame-3.100` folder of the `mp3lame-sys` crate (https://crates.io/crates/mp3lame-sys).
Since Bootleg Toolbox itself is GPL and its whole source is published, you can rebuild the
program with a modified LAME.

## Fonts

The user interface uses Noto Sans Light, Noto Sans Mono and Noto Emoji (copyright The Noto
Project Authors and Google LLC, SIL Open Font Licence 1.1, see
[assets/fonts/OFL.txt](assets/fonts/OFL.txt)), and the emoji-icon-font symbols that egui
ships (MIT, see [assets/fonts/emoji-icon-font-mit-license.txt](assets/fonts/emoji-icon-font-mit-license.txt)).

## ape-decoder (Monkey's Audio decoder), modified

The `ape-decoder` crate 0.3.2 (MIT OR Apache-2.0, https://crates.io/crates/ape-decoder) is
built from a modified copy in [vendor/ape-decoder](vendor/ape-decoder), with its licence
files. The change adds the value coding of Monkey's Audio 3.95 to 3.98 files, which the
original decodes as if they were 3.99 and later; it is listed in its CHANGELOG.md.

## MP3 and AAC transcoding detectors (Lossless?)

The search for MP3 and AAC coding traces in `src/mp3trace.rs` and `src/aactrace.rs` is a
Rust port of the MP3 and AAC detectors of Olivier Derrien (CNRS/PRISM, Marseille, 2018),
https://github.com/craft-and-code/lac-transcoded, described in O. Derrien, "Detection of
genuine lossless audio files. Application to the MPEG-AAC codec", Journal of the Audio
Engineering Society 67(3), 2019. The tables (polyphase window, scale factor bands, aliasing
butterflies) come from that code; Kaiser-Bessel-derived windows were added to the AAC
detector. Licence:

```
MIT License

Copyright (c) 2018 Olivier Derrien

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust crates

Crates built into the Windows and Linux executables (build tools included), by licence.
Each crate's licence text is in its source package on https://crates.io.

### MIT OR Apache-2.0

accesskit v0.16.3, accesskit_atspi_common v0.9.3, accesskit_consumer v0.24.3, accesskit_unix v0.12.3, accesskit_windows v0.23.2, ahash v0.8.12, ape-decoder v0.3.2, arboard v3.7.0, arrayvec v0.7.8, as-raw-xcb-connection v1.0.1, async-broadcast v0.7.2, async-recursion v1.2.0, async-trait v0.1.92, base64 v0.21.7, base64 v0.23.1, bitflags v2.13.2, block-buffer v0.10.4, cc v1.10.0, cfg-if v1.0.5, chrono v0.4.45, cpufeatures v0.2.17, crc v3.4.0, crc-catalog v2.5.0, crc32fast v1.5.3, crossbeam-deque v0.8.8, crossbeam-epoch v0.9.21, crossbeam-utils v0.8.23, crypto-common v0.1.7, digest v0.10.7, displaydoc v0.2.7, document-features v0.2.12, ecolor v0.29.1, eframe v0.29.1, egui v0.29.1, egui-winit v0.29.1, egui_extras v0.29.1, egui_glow v0.29.1, either v1.19.0, emath v0.29.1, enum-map v2.7.3, enum-map-derive v0.17.0, enumflags2 v0.7.12, enumflags2_derive v0.7.12, enumn v0.1.14, epaint v0.29.1, errno v0.3.14, fdeflate v0.3.7, find-msvc-tools v0.1.15, flate2 v1.1.10, form_urlencoded v1.2.2, futures-channel v0.3.34, futures-core v0.3.34, futures-io v0.3.34, futures-macro v0.3.34, futures-sink v0.3.34, futures-task v0.3.34, futures-util v0.3.34, getrandom v0.2.17, getrandom v0.3.4, hash32 v0.3.1, hashbrown v0.17.1, heapless v0.8.0, hex v0.4.3, home v0.5.12, http v1.5.0, httparse v1.10.1, iana-time-zone v0.1.65, idna v1.1.0, image v0.25.10, itoa v1.0.18, lazy_static v1.5.1, libc v0.2.191, litrs v1.0.1, lock_api v0.4.14, log v0.4.34, md-5 v0.10.6, memmap2 v0.9.12, mime v0.3.17, num-traits v0.2.19, once_cell v1.21.4, openssl-probe v0.2.1, ordered-stream v0.2.0, parking_lot v0.12.5, parking_lot_core v0.9.12, paste v1.0.15, percent-encoding v2.3.2, piper v0.2.5, pkg-config v0.3.34, png v0.18.1, ppv-lite86 v0.2.21, proc-macro-crate v3.5.0, proc-macro2 v1.0.107, quote v1.0.47, rand v0.8.8, rand v0.9.5, rand_chacha v0.3.1, rand_chacha v0.9.0, rand_core v0.6.4, rand_core v0.9.5, rayon v1.12.0, rayon-core v1.13.0, regex v1.13.1, regex-automata v0.4.18, regex-syntax v0.8.11, ron v0.8.1, rustls-pki-types v1.15.1, rustls-platform-verifier v0.7.1, rustversion v1.0.23, scopeguard v1.2.0, seq-macro v0.3.6, serde v1.0.229, serde_core v1.0.229, serde_derive v1.0.229, serde_repr v0.1.21, serde_spanned v1.1.2, sha1 v0.10.7, shlex v2.0.1, signal-hook-registry v1.4.8, simdutf8 v0.1.5, siphasher v1.0.4, smallvec v1.16.4, smol_str v0.2.2, stable_deref_trait v1.2.1, static_assertions v1.1.0, syn v2.0.119, syn v3.0.9, thiserror v1.0.69, thiserror v2.0.21, thiserror-impl v1.0.69, thiserror-impl v2.0.21, toml v1.1.8+spec-1.1.0, toml_datetime v1.1.2+spec-1.1.0, toml_edit v0.25.17+spec-1.1.0, toml_parser v1.1.5+spec-1.1.0, ttf-parser v0.25.1, typenum v1.20.1, unicase v2.10.0, unicode-segmentation v1.13.3, ureq v3.5.0, ureq-proto v0.6.5, url v2.5.8, utf8-zero v0.8.1, wavicle v0.1.0, web-time v1.1.0, webbrowser v1.2.5, windows v0.58.0, windows v0.62.2, windows-collections v0.3.2, windows-core v0.58.0, windows-core v0.62.2, windows-future v0.3.2, windows-implement v0.58.0, windows-implement v0.60.2, windows-interface v0.58.0, windows-interface v0.59.3, windows-link v0.2.1, windows-numerics v0.3.1, windows-result v0.2.0, windows-result v0.4.1, windows-strings v0.1.0, windows-strings v0.5.1, windows-sys v0.52.0, windows-sys v0.59.0, windows-sys v0.61.2, windows-targets v0.52.6, windows-threading v0.2.1, windows_x86_64_gnu v0.52.6, x11rb v0.13.2, x11rb-protocol v0.13.2

### MIT

ashpd v0.11.1, autotools v0.2.7, built v0.7.7, bytes v1.12.1, calloop v0.13.0, calloop v0.14.5, calloop-wayland-source v0.3.0, calloop-wayland-source v0.4.1, cfg_aliases v0.2.2, dlib v0.5.3, endi v1.1.1, extended v0.1.0, generic-array v0.14.7, glutin-winit v0.5.0, is-docker v0.2.0, is-wsl v0.4.0, memoffset v0.9.1, mime_guess2 v2.3.1, nix v0.29.0, open v5.4.5, phf v0.11.3, phf_shared v0.11.3, quick-xml v0.30.0, quick-xml v0.41.0, rfd v0.15.4, sctk-adwaita v0.10.1, simd-adler32 v0.3.10, slab v0.4.12, smithay-client-toolkit v0.19.2, smithay-client-toolkit v0.20.0, smithay-clipboard v0.7.3, strict-num v0.1.1, synstructure v0.14.0, tracing v0.1.44, tracing-attributes v0.1.31, tracing-core v0.1.36, trash v5.2.9, urlencoding v2.1.3, wayland-backend v0.3.17, wayland-client v0.31.15, wayland-csd-frame v0.3.0, wayland-cursor v0.31.14, wayland-protocols v0.32.13, wayland-protocols-experimental v20250721.0.1, wayland-protocols-misc v0.3.12, wayland-protocols-plasma v0.3.12, wayland-protocols-wlr v0.3.12, wayland-scanner v0.31.11, wayland-sys v0.31.11, winnow v1.0.4, winresource v0.1.34, x11-dl v2.21.0, xcursor v0.3.11, xdg-home v1.3.0, xkbcommon-dl v0.4.2, xml-rs v0.8.29, zbus v4.4.0, zbus v5.19.0, zbus-lockstep v0.4.4, zbus-lockstep-macros v0.4.4, zbus_macros v4.4.0, zbus_macros v5.19.0, zbus_names v3.0.0, zbus_names v4.3.5, zbus_xml v4.0.0, zcheapstr v1.1.0, zvariant v4.2.0, zvariant v5.15.1, zvariant_derive v4.2.0, zvariant_derive v5.15.1, zvariant_utils v2.1.0, zvariant_utils v4.2.0

### Apache-2.0 OR MIT

async-channel v2.5.0, async-executor v1.14.0, async-fs v2.2.0, async-io v2.6.0, async-lock v3.4.2, async-net v2.0.0, async-process v2.5.0, async-signal v0.2.14, async-task v4.7.1, atomic-waker v1.1.2, atspi v0.22.0, atspi-common v0.6.0, atspi-connection v0.6.0, atspi-proxies v0.6.0, autocfg v1.5.1, blocking v1.7.0, concurrent-queue v2.5.0, equivalent v1.0.3, event-listener v5.4.2, event-listener-strategy v0.5.4, fastrand v2.5.0, futures-lite v2.6.1, idna_adapter v1.2.2, immutable-chunkmap v2.1.4, indexmap v2.14.2, multiversion_no_op v1.0.0, nohash-hasher v0.2.0, parking v2.2.1, pin-project-lite v0.2.17, polling v3.11.0, utf8_iter v1.0.4, uuid v1.29.0, zeroize v1.9.1

### Unicode-3.0

icu_collections v2.4.0, icu_locale_core v2.4.0, icu_normalizer v2.4.0, icu_normalizer_data v2.4.0, icu_properties v2.4.0, icu_properties_data v2.4.0, icu_provider v2.4.0, litemap v0.8.3, potential_utf v0.1.6, tinystr v0.8.5, writeable v0.6.4, yoke v0.8.4, yoke-derive v0.8.4, zerofrom v0.1.8, zerofrom-derive v0.1.8, zerotrie v0.2.5, zerovec v0.11.9, zerovec-derive v0.11.7

### Apache-2.0

ab_glyph v0.2.32, ab_glyph_rasterizer v0.1.10, accesskit_winit v0.22.4, flacenc v0.5.1, gethostname v1.1.0, gl_generator v0.14.0, glutin v0.32.3, glutin_egl_sys v0.7.1, glutin_glx_sys v0.6.1, glutin_wgl_sys v0.6.1, khronos_api v3.1.0, owned_ttf_parser v0.25.1, winit v0.30.13

### MPL-2.0

symphonia v0.5.5, symphonia-bundle-flac v0.5.5, symphonia-bundle-mp3 v0.5.5, symphonia-codec-aac v0.5.5, symphonia-codec-alac v0.5.5, symphonia-codec-pcm v0.5.5, symphonia-codec-vorbis v0.5.5, symphonia-core v0.5.5, symphonia-format-isomp4 v0.5.5, symphonia-format-mkv v0.5.5, symphonia-format-riff v0.5.5, symphonia-metadata v0.5.5, symphonia-utils-xiph v0.5.5

### MIT/Apache-2.0

bitflags v1.3.2, core_detect v1.0.0, downcast-rs v1.2.1, scoped-tls v1.0.1, version_check v0.9.5, winapi v0.3.9, winapi-x86_64-pc-windows-gnu v0.4.0

### Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

linux-raw-sys v0.12.1, linux-raw-sys v0.4.15, rustix v0.38.44, rustix v1.1.5

### MIT OR Apache-2.0 OR Zlib

cursor-icon v1.2.0, glow v0.14.2, raw-window-handle v0.6.2, xkeysym v0.2.1

### Unlicense OR MIT

aho-corasick v1.1.5, byteorder v1.5.0, byteorder-lite v0.1.0, memchr v2.8.3

### BSD-3-Clause

subtle v2.6.1, tiny-skia v0.11.4, tiny-skia-path v0.11.4

### ISC

libloading v0.8.9, rustls-webpki v0.103.16, untrusted v0.9.0

### Apache-2.0 OR ISC OR MIT

rustls v0.23.46, rustls-native-certs v0.8.5

### BSD-3-Clause OR Apache-2.0

moxcms v0.8.1, pxfm v0.1.30

### BSL-1.0

clipboard-win v5.4.3, error-code v3.4.0

### LGPL-3.0

mp3lame-encoder v0.2.5, mp3lame-sys v0.1.11

### MIT OR Zlib OR Apache-2.0

miniz_oxide v0.8.9, miniz_oxide v0.9.1

### Zlib OR Apache-2.0 OR MIT

bytemuck v1.25.2, bytemuck_derive v1.12.1

### (Apache-2.0 OR MIT) AND BSD-3-Clause

encoding_rs v0.8.42

### (MIT OR Apache-2.0) AND Unicode-3.0

unicode-ident v1.0.27

### 0BSD OR MIT OR Apache-2.0

adler2 v2.0.1

### Apache-2.0 AND ISC

ring v0.17.14

### Apache-2.0 AND MIT

dpi v0.1.2

### Apache-2.0/MIT

pollster v0.4.0

### BSD-2-Clause

arrayref v0.3.9

### BSD-2-Clause OR Apache-2.0 OR MIT

zerocopy v0.8.63
