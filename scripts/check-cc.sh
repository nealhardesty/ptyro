#!/bin/sh
# A stand-in C compiler for `make check-macos`: `cargo check` never links, so C code bundled by
# dependencies (SQLite, zstd, criterion's alloca) only has to "compile". It answers the cc
# crate's compiler probe as clang, writes an empty object wherever the output goes (`-o F` or
# `-FoF`), and succeeds, so build scripts pass without Apple's SDK.
out=
while [ $# -gt 0 ]; do
  case $1 in
    -E) echo '#pragma message "clang"'; echo '#pragma message "clang"' >&2; exit 0;;
    -o) out=$2; shift;;
    -Fo*) out=${1#-Fo};;
  esac
  shift
done
[ -n "$out" ] && : > "$out"
exit 0
