#!/usr/bin/env python3
"""Comprueba ejecutables de Windows (PE) de Tesela: arquitectura e imports.

Uso (lo llama scripts/build-release.ps1; ejecutar con `python -I`):

    python -I scripts/check-pe.py --machine x64 ruta1.exe ruta2.exe

Falla (codigo 1) si algun ejecutable:
  * no es un PE valido,
  * es de otra arquitectura que la esperada (--machine x64|arm64|x86),
  * importa (tabla de imports normal o de carga retardada) alguna de estas DLL del runtime de C: `VCRUNTIME*.dll`,
    `MSVCP*.dll`, `api-ms-win-crt-*.dll` (y `UCRTBASE.dll`, que es lo que arrastran). En Windows 10/11 el UCRT es del
    sistema, pero el release se enlaza estatico (.cargo/config.toml) para no depender del runtime de VC++ redistribuible
    (`VCRUNTIME*`, `MSVCP*`), que el instalador por usuario y sin administrador no puede instalar,
  * importa (tabla normal o de carga retardada) algun modulo de red que no sea `winhttp.dll`: `ws2_32`, `wsock32`, `mswsock`,
    `wininet`, `urlmon`, `iphlpapi`, `dnsapi`, `secur32`, `crypt32`, `httpapi`, `netapi32`, `wldap32`, `rasapi32`. El unico
    codigo de Tesela que puede llegar a la red es la comprobacion de actualizaciones opcional (crate `tesela-update`, por
    WinHTTP); si algun dia una dependencia arrastra otro modulo de red, que sea una decision explicita y no un descuido,
  * no lleva `DependentLoadFlags` = 0x800 (`LOAD_LIBRARY_SEARCH_SYSTEM32`, el de `/DEPENDENTLOADFLAG:0x800`) en el
    directorio de configuracion de carga: sin el, las importaciones estaticas se buscan primero en la carpeta del .exe,
  * no lleva una entrada de depuracion `IMAGE_DEBUG_TYPE_REPRO` (la que escribe `/Brepro`): sin ella el enlace no es
    reproducible (lleva una marca de tiempo).

Solo lee los archivos. Solo usa la biblioteca estandar.
"""

import argparse
import fnmatch
import struct
import sys

MACHINES = {"x86": 0x014C, "x64": 0x8664, "arm64": 0xAA64}
# `DependentLoadFlags` que escribe `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32).
WANT_LOAD_FLAGS = 0x800
IMAGE_DEBUG_TYPE_REPRO = 16
# Patrones (sin distinguir mayusculas) de DLL del runtime de C que no se admiten como import.
FORBIDDEN = ["vcruntime*.dll", "msvcp*.dll", "api-ms-win-crt-*.dll", "ucrtbase.dll", "msvcr*.dll", "concrt*.dll"]
# Modulos de red (sin distinguir mayusculas) que no se admiten como import: el unico permitido es `winhttp.dll` (la
# comprobacion de actualizaciones opcional; ver `crates/update`).
FORBIDDEN_NETWORK = [
    "ws2_32.dll",
    "wsock32.dll",
    "mswsock.dll",
    "wininet.dll",
    "urlmon.dll",
    "iphlpapi.dll",
    "dnsapi.dll",
    "secur32.dll",
    "crypt32.dll",
    "httpapi.dll",
    "netapi32.dll",
    "wldap32.dll",
    "rasapi32.dll",
]


class PeError(Exception):
    """El archivo no es un PE que se pueda leer."""


def _u16(b, o):
    if o < 0 or o + 2 > len(b):
        raise PeError("lectura fuera de rango")
    return struct.unpack_from("<H", b, o)[0]


def _u32(b, o):
    if o < 0 or o + 4 > len(b):
        raise PeError("lectura fuera de rango")
    return struct.unpack_from("<I", b, o)[0]


def parse(data):
    """Devuelve (machine, [DLL importadas], [DLL de carga retardada]) de los bytes de un PE."""
    info = parse_info(data)
    return info["machine"], info["imports"], info["delay"]


def parse_info(data):
    """Todo lo que lee este script de un PE: machine, imports, delay, `load_flags` (`DependentLoadFlags`, o None si el
    directorio de configuracion de carga no lo trae) y `repro` (hay una entrada de depuracion REPRO)."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PeError("no es un PE (falta la firma MZ)")
    pe = _u32(data, 0x3C)
    if data[pe:pe + 4] != b"PE\0\0":
        raise PeError("no es un PE (falta la firma PE)")
    machine = _u16(data, pe + 4)
    nsec = _u16(data, pe + 6)
    opt_size = _u16(data, pe + 20)
    opt = pe + 24
    magic = _u16(data, opt)
    if magic == 0x20B:  # PE32+
        dd = opt + 112
        image_base = struct.unpack_from("<Q", data, opt + 24)[0]
    elif magic == 0x10B:  # PE32
        dd = opt + 96
        image_base = _u32(data, opt + 28)
    else:
        raise PeError("cabecera opcional desconocida")
    ndd = _u32(data, dd - 4)
    sec_off = opt + opt_size
    sections = []
    for i in range(nsec):
        s = sec_off + 40 * i
        vsize = _u32(data, s + 8)
        va = _u32(data, s + 12)
        rsize = _u32(data, s + 16)
        raw = _u32(data, s + 20)
        sections.append((va, max(vsize, rsize), raw, rsize))

    def rva_to_off(rva):
        for va, size, raw, rsize in sections:
            if va <= rva < va + size:
                delta = rva - va
                # Entre `rsize` y `vsize` (relleno de ceros en memoria) no hay bytes en el archivo: no se lee de la
                # siguiente seccion por casualidad.
                if delta >= rsize:
                    raise PeError("RVA fuera de los datos de la seccion en el archivo")
                return raw + delta
        raise PeError("RVA fuera de las secciones")

    def cstr(rva):
        o = rva_to_off(rva)
        end = data.find(b"\0", o)
        if end < 0:
            raise PeError("cadena sin terminar")
        return data[o:end].decode("ascii", "replace")

    def directory(index):
        if index >= ndd:
            return 0, 0
        return _u32(data, dd + 8 * index), _u32(data, dd + 8 * index + 4)

    imports = []
    rva, size = directory(1)  # IMAGE_DIRECTORY_ENTRY_IMPORT
    if rva and size:
        o = rva_to_off(rva)
        for _ in range(4096):
            name_rva = _u32(data, o + 12)
            if name_rva == 0 and _u32(data, o) == 0 and _u32(data, o + 16) == 0:
                break
            imports.append(cstr(name_rva))
            o += 20
        else:
            raise PeError("demasiados imports")
    delay = []
    rva, size = directory(13)  # IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT
    if rva and size:
        o = rva_to_off(rva)
        for _ in range(4096):
            attrs = _u32(data, o)
            name_rva = _u32(data, o + 4)
            if attrs == 0 and name_rva == 0:
                break
            # Con el bit 0 de `attrs` las direcciones son RVA; sin el (formato antiguo de VC6) son direcciones virtuales
            # absolutas: se convierten con la base de la imagen. Un descriptor NUNCA se ignora: o se interpreta o falla.
            if not attrs & 1:
                if name_rva < image_base:
                    raise PeError("descriptor de carga retardada con una direccion fuera de la imagen")
                name_rva -= image_base
            delay.append(cstr(name_rva))
            o += 32
        else:
            raise PeError("demasiados imports de carga retardada")
    # Directorio de configuracion de carga (indice 10): `DependentLoadFlags` esta en 0x4E (PE32+) o 0x36 (PE32) y solo vale si
    # el campo `Size` del directorio lo cubre.
    load_flags = None
    rva, size = directory(10)  # IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG
    if rva and size:
        o = rva_to_off(rva)
        field = 0x4E if magic == 0x20B else 0x36
        if _u32(data, o) >= field + 2 and size >= field + 2:
            load_flags = _u16(data, o + field)
    # Directorio de depuracion (indice 6): entradas de 28 bytes; el tipo esta en +12.
    repro = False
    rva, size = directory(6)  # IMAGE_DIRECTORY_ENTRY_DEBUG
    if rva and size:
        o = rva_to_off(rva)
        for k in range(min(size // 28, 64)):
            if _u32(data, o + 28 * k + 12) == IMAGE_DEBUG_TYPE_REPRO:
                repro = True
    return {"machine": machine, "imports": imports, "delay": delay, "load_flags": load_flags, "repro": repro}


def forbidden(names):
    """Las DLL de `names` que son del runtime de C (sin distinguir mayusculas)."""
    out = []
    for n in names:
        low = n.lower()
        if any(fnmatch.fnmatchcase(low, pat) for pat in FORBIDDEN):
            out.append(n)
    return out


def network_modules(names):
    """Los modulos de red de `names` que no se admiten (todos menos `winhttp.dll`), sin distinguir mayusculas."""
    return [n for n in names if n.lower() in FORBIDDEN_NETWORK]


def check(path, machine_name):
    """Lista de problemas de un ejecutable (vacia si esta bien)."""
    try:
        with open(path, "rb") as f:
            data = f.read()
        info = parse_info(data)
    except (OSError, PeError, struct.error) as e:
        return [f"{path}: {e}"]
    machine, imports, delay = info["machine"], info["imports"], info["delay"]
    problems = []
    want = MACHINES[machine_name]
    if machine != want:
        problems.append(f"{path}: arquitectura 0x{machine:04X}, se esperaba {machine_name} (0x{want:04X})")
    bad = forbidden(imports + delay)
    if bad:
        problems.append(f"{path}: importa el runtime de C: {', '.join(sorted(set(bad), key=str.lower))}")
    net = network_modules(imports + delay)
    if net:
        problems.append(
            f"{path}: importa un modulo de red que no es winhttp.dll: {', '.join(sorted(set(net), key=str.lower))}"
        )
    flags = info["load_flags"]
    if flags != WANT_LOAD_FLAGS:
        got = "ausente" if flags is None else f"0x{flags:04X}"
        problems.append(
            f"{path}: DependentLoadFlags {got}, se esperaba 0x{WANT_LOAD_FLAGS:04X} (/DEPENDENTLOADFLAG:0x800)"
        )
    if not info["repro"]:
        problems.append(f"{path}: sin entrada de depuracion REPRO (falta /Brepro: el enlace no es reproducible)")
    return problems


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--machine", choices=sorted(MACHINES), required=True)
    ap.add_argument("--list", action="store_true", help="imprime tambien los imports de cada ejecutable")
    ap.add_argument("exes", nargs="+")
    args = ap.parse_args(argv)
    problems = []
    for path in args.exes:
        problems += check(path, args.machine)
        if args.list:
            try:
                with open(path, "rb") as f:
                    _, imports, delay = parse(f.read())
                print(f"{path}: imports = {', '.join(imports)}" + (f"; retardados = {', '.join(delay)}" if delay else ""))
            except (OSError, PeError, struct.error):
                pass
    if problems:
        for p in problems:
            print(p, file=sys.stderr)
        return 1
    print(
        f"check-pe: OK ({len(args.exes)} ejecutables, {args.machine}, sin imports del runtime de C, "
        "sin modulos de red salvo winhttp.dll, DependentLoadFlags 0x800 y enlace reproducible)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
