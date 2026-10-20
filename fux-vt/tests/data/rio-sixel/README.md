Three sixel pictures and the RGBA pixels each decodes to, from rio's
`rio-vt/tests/sixel` at commit 59203ccd5fa1e6b760a738c1ad653bb29f2f3465
(https://github.com/raphamorim/rio), MIT licensed (LICENSE here). The same
64 x 64 test image, encoded by ImageMagick 6 (`im6`), libsixel and
netpbm's `ppmtosixel`. `tests/sixel_pairs.rs` decodes each through
fux-vt's parser and compares the pixels with the `.rgba` beside it.
