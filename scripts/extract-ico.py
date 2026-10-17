#!/usr/bin/env python3
"""Extract the icon from a Windows executable (PE) to an .ico file.

Usage (called by scripts/build-msi.ps1; run with `python -I`):

    python -I scripts/extract-ico.py tesela.exe salida.ico

Why this exists: the MSI needs an icon file for "Installed Applications" and the shortcut. Pointing the table
Icon del MSI al propio .exe incrustaria una copia ENTERA del ejecutable (el MSI pesaria el doble). El icono se dibuja en
cada compilacion (crates/app/build.rs) y el repositorio no versiona imagenes, asi que se saca del .exe ya compilado: es
el mismo icono que lleva el ejecutable, no una copia aparte.

Reads the first RT_GROUP_ICON resource and its referenced RT_ICON resources, then writes the ICO container. Reads the
executable only and uses the standard library only.
"""

import struct
import sys

RT_ICON = 3
RT_GROUP_ICON = 14
# A normal resource tree has three levels (type, name, language): more than four indicates a malformed PE or a cycle.
MAX_DEPTH = 4


class PeError(Exception):
    """The file is not a readable PE with an icon."""


def _check(data, off, n):
    if off < 0 or off + n > len(data):
        raise PeError("read out of range")


def extract(data):
    """Return .ico bytes for the first icon group in PE resources from `data`.

    A truncated or malformed PE (out-of-range reads, cycles in the resource tree) raises `PeError`, never loops
    infinito ni otra excepcion.
    """
    try:
        return _extract(data)
    except struct.error as e:
        raise PeError(f"PE truncado o no valido: {e}") from e


def _extract(data):
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PeError("not a PE (missing MZ signature)")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    _check(data, pe, 24)
    if data[pe:pe + 4] != b"PE\0\0":
        raise PeError("not a PE (missing PE signature)")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic == 0x20B:
        dd = opt + 112
    elif magic == 0x10B:
        dd = opt + 96
    else:
        raise PeError("unknown optional header")
    ndd = struct.unpack_from("<I", data, dd - 4)[0]
    if ndd < 3:
        raise PeError("the executable has no resources")
    res_rva, res_size = struct.unpack_from("<II", data, dd + 8 * 2)  # IMAGE_DIRECTORY_ENTRY_RESOURCE
    if not res_rva or not res_size:
        raise PeError("the executable has no resources")
    sections = []
    for i in range(nsec):
        s = opt + opt_size + 40 * i
        _check(data, s, 40)
        vsize, va, rsize, raw = struct.unpack_from("<IIII", data, s + 8)
        sections.append((va, max(vsize, rsize), raw, rsize))

    def rva_to_off(rva):
        for va, size, raw, rsize in sections:
            if va <= rva < va + size:
                delta = rva - va
                # Between `rsize` and `vsize`, no file bytes exist (the remainder is zero-filled in memory).
                if delta >= rsize:
                    raise PeError("RVA is outside the section's file data")
                return raw + delta
        raise PeError("RVA is outside all sections")

    base = rva_to_off(res_rva)

    def entries(off):
        """Resource directory entries as [(id, target)]; text names are ignored."""
        _check(data, off, 16)
        nnamed, nid = struct.unpack_from("<HH", data, off + 12)
        out = []
        for k in range(nnamed + nid):
            ident, target = struct.unpack_from("<II", data, off + 16 + 8 * k)
            if k >= nnamed:
                out.append((ident, target))
        return out

    def leaf(target):
        """Descend through the name and language directories to the first resource's bytes."""
        seen = set()
        depth = 1  # The root directory (types) is already level 1.
        while target & 0x80000000:
            off = target & 0x7FFFFFFF
            depth += 1
            if off in seen or depth > MAX_DEPTH or off >= res_size:
                raise PeError("resource tree contains a cycle or is too deep")
            seen.add(off)
            sub = entries(base + off)
            if not sub:
                raise PeError("empty resource directory")
            target = sub[0][1]
        _check(data, base + target, 16)
        rva, size = struct.unpack_from("<II", data, base + target)
        o = rva_to_off(rva)
        _check(data, o, size)
        return data[o:o + size]

    types = dict(entries(base))
    if RT_GROUP_ICON not in types or RT_ICON not in types:
        raise PeError("the executable has no icon (RT_GROUP_ICON / RT_ICON)")
    group = leaf(types[RT_GROUP_ICON])
    icons = {}
    if not types[RT_ICON] & 0x80000000 or not types[RT_GROUP_ICON] & 0x80000000:
        raise PeError("icon resources are not directories")
    for ident, target in entries(base + (types[RT_ICON] & 0x7FFFFFFF)):
        icons[ident] = leaf(target)
    reserved, kind, count = struct.unpack_from("<HHH", group, 0)
    if kind != 1 or count == 0 or len(group) < 6 + 14 * count:
        raise PeError("invalid icon group")
    out = bytearray(struct.pack("<HHH", 0, 1, count))
    blobs = []
    offset = 6 + 16 * count
    for i in range(count):
        w, h, colors, res, planes, bits, nbytes, ident = struct.unpack_from("<BBBBHHIH", group, 6 + 14 * i)
        if ident not in icons or len(icons[ident]) != nbytes:
            raise PeError(f"missing image {ident} from the icon group")
        out += struct.pack("<BBBBHHII", w, h, colors, res, planes, bits, nbytes, offset)
        blobs.append(icons[ident])
        offset += nbytes
    for b in blobs:
        out += b
    return bytes(out)


def main(argv):
    if len(argv) != 2:
        print("usage: extract-ico.py <executable.exe> <output.ico>", file=sys.stderr)
        return 2
    try:
        with open(argv[0], "rb") as f:
            ico = extract(f.read())
    except (OSError, PeError, struct.error) as e:
        print(f"extract-ico: {argv[0]}: {e}", file=sys.stderr)
        return 1
    with open(argv[1], "wb") as f:
        f.write(ico)
    count = struct.unpack_from("<H", ico, 4)[0]
    print(f"extract-ico: OK ({count} images, {len(ico)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
