#!/usr/bin/env python3
"""Extrae el icono de un ejecutable de Windows (PE) a un archivo .ico.

Uso (lo llama scripts/build-msi.ps1; ejecutar con `python -I`):

    python -I scripts/extract-ico.py tesela.exe salida.ico

Por que existe: el MSI necesita un archivo de icono para "Aplicaciones instaladas" y el acceso directo. Apuntar la tabla
Icon del MSI al propio .exe incrustaria una copia ENTERA del ejecutable (el MSI pesaria el doble). El icono se dibuja en
cada compilacion (crates/app/build.rs) y el repositorio no versiona imagenes, asi que se saca del .exe ya compilado: es
el mismo icono que lleva el ejecutable, no una copia aparte.

Lee el primer recurso RT_GROUP_ICON y los RT_ICON que referencia, y escribe el contenedor ICO. Solo lee el ejecutable.
Solo usa la biblioteca estandar.
"""

import struct
import sys

RT_ICON = 3
RT_GROUP_ICON = 14
# Un arbol de recursos real tiene 3 niveles (tipo, nombre, idioma): mas de 4 es un PE manipulado (o con un ciclo).
MAX_DEPTH = 4


class PeError(Exception):
    """El archivo no es un PE con un icono que se pueda leer."""


def _check(data, off, n):
    if off < 0 or off + n > len(data):
        raise PeError("lectura fuera de rango")


def extract(data):
    """Devuelve los bytes de un .ico con el primer grupo de iconos de los recursos del PE `data`.

    Un PE truncado o manipulado (lecturas fuera de rango, ciclos en el arbol de recursos) da `PeError`, nunca un bucle
    infinito ni otra excepcion.
    """
    try:
        return _extract(data)
    except struct.error as e:
        raise PeError(f"PE truncado o no valido: {e}") from e


def _extract(data):
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PeError("no es un PE (falta la firma MZ)")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    _check(data, pe, 24)
    if data[pe:pe + 4] != b"PE\0\0":
        raise PeError("no es un PE (falta la firma PE)")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    if magic == 0x20B:
        dd = opt + 112
    elif magic == 0x10B:
        dd = opt + 96
    else:
        raise PeError("cabecera opcional desconocida")
    ndd = struct.unpack_from("<I", data, dd - 4)[0]
    if ndd < 3:
        raise PeError("el ejecutable no tiene recursos")
    res_rva, res_size = struct.unpack_from("<II", data, dd + 8 * 2)  # IMAGE_DIRECTORY_ENTRY_RESOURCE
    if not res_rva or not res_size:
        raise PeError("el ejecutable no tiene recursos")
    sections = []
    for i in range(nsec):
        s = opt + opt_size + 40 * i
        _check(data, s, 40)
        vsize, va, rsize, raw = struct.unpack_from("<IIII", data, s + 8)
        sections.append((va, max(vsize, rsize), raw))

    def rva_to_off(rva):
        for va, size, raw in sections:
            if va <= rva < va + size:
                return raw + (rva - va)
        raise PeError("RVA fuera de las secciones")

    base = rva_to_off(res_rva)

    def entries(off):
        """Entradas de un directorio de recursos: [(id, destino)] (los nombres de texto se ignoran)."""
        _check(data, off, 16)
        nnamed, nid = struct.unpack_from("<HH", data, off + 12)
        out = []
        for k in range(nnamed + nid):
            ident, target = struct.unpack_from("<II", data, off + 16 + 8 * k)
            if k >= nnamed:
                out.append((ident, target))
        return out

    def leaf(target):
        """Baja por los directorios (nombre, idioma) hasta los bytes del primer recurso."""
        seen = set()
        depth = 1  # el directorio raiz (tipos) ya es el nivel 1
        while target & 0x80000000:
            off = target & 0x7FFFFFFF
            depth += 1
            if off in seen or depth > MAX_DEPTH or off >= res_size:
                raise PeError("arbol de recursos con un ciclo o demasiado profundo")
            seen.add(off)
            sub = entries(base + off)
            if not sub:
                raise PeError("directorio de recursos vacio")
            target = sub[0][1]
        _check(data, base + target, 16)
        rva, size = struct.unpack_from("<II", data, base + target)
        o = rva_to_off(rva)
        _check(data, o, size)
        return data[o:o + size]

    types = dict(entries(base))
    if RT_GROUP_ICON not in types or RT_ICON not in types:
        raise PeError("el ejecutable no lleva icono (RT_GROUP_ICON / RT_ICON)")
    group = leaf(types[RT_GROUP_ICON])
    icons = {}
    if not types[RT_ICON] & 0x80000000 or not types[RT_GROUP_ICON] & 0x80000000:
        raise PeError("los recursos de icono no son directorios")
    for ident, target in entries(base + (types[RT_ICON] & 0x7FFFFFFF)):
        icons[ident] = leaf(target)
    reserved, kind, count = struct.unpack_from("<HHH", group, 0)
    if kind != 1 or count == 0 or len(group) < 6 + 14 * count:
        raise PeError("grupo de iconos no valido")
    out = bytearray(struct.pack("<HHH", 0, 1, count))
    blobs = []
    offset = 6 + 16 * count
    for i in range(count):
        w, h, colors, res, planes, bits, nbytes, ident = struct.unpack_from("<BBBBHHIH", group, 6 + 14 * i)
        if ident not in icons or len(icons[ident]) != nbytes:
            raise PeError(f"falta la imagen {ident} del grupo de iconos")
        out += struct.pack("<BBBBHHII", w, h, colors, res, planes, bits, nbytes, offset)
        blobs.append(icons[ident])
        offset += nbytes
    for b in blobs:
        out += b
    return bytes(out)


def main(argv):
    if len(argv) != 2:
        print("uso: extract-ico.py <ejecutable.exe> <salida.ico>", file=sys.stderr)
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
    print(f"extract-ico: OK ({count} imagenes, {len(ico)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
