# Third-Party Notices

Binaries built with `cf-gears-file-parser` embed libpdfium, the PDF engine used
by the PDF parser. The library is the prebuilt release
[`chromium/7961`](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/7961)
from bblanchon/pdfium-binaries. `build.rs` embeds it for the targets listed in
the [README](README.md#bundled-libpdfium). Builds without a bundled library
(musl, other targets, docs.rs) do not contain it.

libpdfium contains the components below. The full licence texts are in the
`licenses/` directory of each release archive, available at
<https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/7961>.

| Component | Licence | Copyright | File in `licenses/` |
|---|---|---|---|
| PDFium | BSD-3-Clause (full text below; the file also carries the Apache License 2.0 text) | 2014 The PDFium Authors | `pdfium.txt` |
| Abseil | Apache-2.0 | not stated in the file | `abseil.txt` |
| Anti-Grain Geometry 2.3 | AGG 2.3 permissive notice (copy, use, modify, sell and distribute, provided the copyright notice appears in all copies) | 2002-2005 Maxim Shemanarev | `agg23.txt` |
| fast_float | MIT | 2021 The fast_float authors | `fast_float.txt` |
| FreeType | FreeType License (FTL) | 1996-2002, 2006 David Turner, Robert Wilhelm, and Werner Lemberg | `freetype.txt` |
| ICU | Unicode License V3 (Unicode-3.0), with ICU's third-party notices in the same file | 2016-2025 Unicode, Inc. | `icu.txt` |
| Little CMS | MIT | 1998-2026 Marti Maria Saguer | `lcms.txt` |
| libjpeg-turbo | IJG License, BSD-3-Clause and Zlib | 1991-2020 Thomas G. Lane, Guido Vollbeding; 2009-2024 D. R. Commander; 2015 Viktor Szathmáry | `libjpeg_turbo.ijg`, `libjpeg_turbo.md` |
| OpenJPEG | BSD-2-Clause | 2005 Herve Drolon, FreeImage Team; 2008, 2011-2012 CNES; 2012 CS Systemes d'Information | `libopenjpeg.txt` |
| libpng | PNG Reference Library License version 2 (the file also carries version 1, for libpng 0.5 through 1.6.35) | 1995-2026 The PNG Reference Library Authors, and others listed in the file | `libpng.txt` |
| libtiff | libtiff License | 1988-1997 Sam Leffler; 1991-1997 Silicon Graphics, Inc. | `libtiff.txt` |
| LLVM libc | Apache-2.0 WITH LLVM-exception (the file also carries the legacy University of Illinois/NCSA licence) | 2007-2019 University of Illinois at Urbana-Champaign (legacy licence) | `llvm-libc.txt` |
| simdutf | MIT | 2021 The simdutf authors | `simdutf.txt` |
| zlib | Zlib | 1995-2026 Jean-loup Gailly and Mark Adler | `zlib.txt` |

The packaging scripts of pdfium-binaries are MIT-licensed (Copyright 2014-2025
Benoit Blanchon); see `LICENSE` at the top of the release archive.

## Required acknowledgements

Portions of this software are copyright © 2006 The FreeType Project
(https://freetype.org). All rights reserved.

This software is based in part on the work of the Independent JPEG Group.

## PDFium licence

```text
Copyright 2014 The PDFium Authors

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * Neither the name of Google Inc. nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
