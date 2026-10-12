#!/usr/bin/env python3
"""Replace text in a Unicode Outlook archive (.pst) with text of the same length.

Outlook writes the signed-in account's address into every item it saves (last modified by), and
the store sets it again on each save, so the test archive is scrubbed after it is made. Each data
block that contains the text is decoded (compressible encoding), changed, encoded again and given
a new CRC; sizes and offsets stay as they are, so the archive stays valid. The CRC code is
checked against every untouched block first.

Usage: python scripts/scrub-pst.py ARCHIVE.pst TEXT REPLACEMENT [TEXT REPLACEMENT ...]
Each TEXT is replaced as ASCII and as UTF-16, case-insensitively.
"""

import re
import struct
import sys
import zlib

# [MS-PST] 5.1 mpbbCrypt, first 256 bytes: the compressible encoding (byte substitution).
ENCODE = bytes.fromhex(
    "41361362a8216ebbf416cc047f64e85d1ef2cb2a74c55e35d295479e962d9a88"
    "4c7d843fdbac31b6485ff6c4d8398be7233b388ec8c1df25b120a546604e9cfb"
    "aad35651457c550007c92b9d859b09a08fadb30f63ab894bd7a7155a716642bf"
    "264a6b98faea7753b270052cfd593a867ece06eb827857c78d43afb41cd45bcd"
    "e2e9274fc3087280cfb0eff5286dbe304d3492d50e3c2232e5e4f99fc2d10a81"
    "12e1ee918376e397e6618a1779a4b7dc907a5c8c02a6ca69de501a1193b95287"
    "58fced1d37491b6ae0293399bd6cd994f340546ff0c673b8d63e6518441fdd67"
    "10f10c19ecae03a1147ba90bfff8a3c0a201f72ebc2468750dfeba2fb5d0da3d"
)
DECODE = bytes(ENCODE.index(b) for b in range(256))

PAGE = 512


def crc(data: bytes) -> int:
    """[MS-PST] 5.3: CRC-32 with the usual table, starting from 0 and without the final XOR."""
    return ~zlib.crc32(data, 0xFFFFFFFF) & 0xFFFFFFFF


def blocks(pst: bytes):
    """(ib, cb, bid) of every block, from the block B-tree (Unicode format)."""
    ib_root = struct.unpack_from("<Q", pst, 240)[0]  # ROOT.BREFBBT.ib
    stack = [ib_root]
    while stack:
        page = pst[stack.pop():][:PAGE]
        c_ent, _, cb_ent, level = page[488:492]
        for k in range(c_ent):
            e = page[k * cb_ent:(k + 1) * cb_ent]
            if level > 0:
                stack.append(struct.unpack_from("<Q", e, 16)[0])  # BTENTRY.BREF.ib
            else:
                bid, ib, cb = struct.unpack_from("<QQH", e)  # BBTENTRY
                yield ib, cb, bid


def main(path: str, pairs: list[tuple[str, str]]) -> None:
    pst = bytearray(open(path, "rb").read())
    if pst[:4] != b"!BDN" or struct.unpack_from("<H", pst, 10)[0] != 23:
        sys.exit("not a Unicode .pst")
    encoded = pst[513] == 1  # bCryptMethod: NDB_CRYPT_PERMUTE
    patterns = []
    for text, repl in pairs:
        if len(text) != len(repl):
            sys.exit(f"{repl!r} must be as long as the text it replaces")
        for enc in ("ascii", "utf-16-le"):
            patterns.append((re.compile(re.escape(text.encode(enc)), re.I), repl.encode(enc)))

    changed = 0
    for ib, cb, bid in blocks(bytes(pst)):
        trailer = ib + ((cb + 16 + 63) // 64) * 64 - 16
        stored_crc = struct.unpack_from("<I", pst, trailer + 4)[0]
        raw = bytes(pst[ib:ib + cb])
        if crc(raw) != stored_crc:
            sys.exit(f"block {bid:#x} at {ib:#x}: CRC does not match; not touching the file")
        internal = bid & 2  # XBLOCK / SLBLOCK: never encoded
        data = raw.translate(DECODE) if encoded and not internal else raw
        new = data
        for rx, repl in patterns:
            new = rx.sub(repl, new)
        if new != data:
            out = new.translate(ENCODE) if encoded and not internal else new
            pst[ib:ib + cb] = out
            struct.pack_into("<I", pst, trailer + 4, crc(out))
            changed += 1
    open(path, "wb").write(pst)
    print(f"{path}: {changed} block(s) changed")


if __name__ == "__main__":
    args = sys.argv[1:]
    if len(args) < 3 or len(args) % 2 == 0:
        sys.exit(__doc__)
    main(args[0], list(zip(args[1::2], args[2::2])))
