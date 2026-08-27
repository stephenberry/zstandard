# Attribution

`zstandard` is an independent implementation of the Zstandard compression format in
Rust. It does not link, vendor, or transliterate the reference C library, and it
is not a binding to it.

It is, however, written against that library. The format itself comes from
[RFC 8878](https://www.rfc-editor.org/rfc/rfc8878.html), but a compressor is
more than its format: the parts that decide *how well* it compresses are
heuristics, parameter tables, and cost models that the specification does not
describe and that exist only in the reference implementation. Where matching
upstream's output was the goal, those were studied and reproduced. Examples
include the compression-level parameter tables, the Huffman decoder's
table-shape cost model, and various parser acceptance thresholds.

Reasonable people can disagree about whether functional tables of that kind
carry copyright. This file exists so that the question does not have to be
settled in order to use the crate: upstream's notice is reproduced below and
travels with every copy of this source, which is what its license asks for.

The reference implementation is developed by Meta Platforms and is available at
<https://github.com/facebook/zstd>. The revision this crate is tested and
benchmarked against is recorded in `upstream-zstd.ref`.

Upstream is dual licensed, BSD or GPL-2.0, at the user's option. Its BSD terms
follow in full.

---

BSD License

For Zstandard software

Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

 * Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

 * Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

 * Neither the name Facebook, nor Meta, nor the names of its contributors may
   be used to endorse or promote products derived from this software without
   specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
