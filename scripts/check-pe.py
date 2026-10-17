#!/usr/bin/env python3
"""Check Tesela Windows executables (PE): architecture and imports.

Usage (called by scripts/build-release.ps1; run with `python -I`):

    python -I scripts/check-pe.py --machine x64 file1.exe file2.exe

Fails (exit code 1) if an executable:
  * is not a valid PE,
  * has an architecture other than the expected one (--machine x64|arm64|x86),
  * imports (normal or delay-load import table) any of these C runtime DLLs: `VCRUNTIME*.dll`,
    `MSVCP*.dll`, `api-ms-win-crt-*.dll` (and `UCRTBASE.dll`, which they depend on). On Windows 10/11, UCRT is part of
    the system, but the release uses static linking (.cargo/config.toml) to avoid the redistributable VC++ runtime,
    which the per-user installer cannot install without administrator rights;
  * imports (normal or delay-load table) a network module other than `winhttp.dll`: `ws2_32`, `wsock32`, `mswsock`,
    `wininet`, `urlmon`, `iphlpapi`, `dnsapi`, `secur32`, `crypt32`, `httpapi`, `netapi32`, `wldap32`, `rasapi32`. The
    only Tesela code that may access the network is the optional update check (the `tesela-update` crate, through
    WinHTTP); if a dependency ever brings in another network module, that must be an explicit decision;
  * does not set `DependentLoadFlags` = 0x800 (`LOAD_LIBRARY_SEARCH_SYSTEM32`, from `/DEPENDENTLOADFLAG:0x800`) in the
    load configuration directory; without it, static imports are searched first in the .exe directory;
  * does not contain an `IMAGE_DEBUG_TYPE_REPRO` debug entry (written by `/Brepro`), which is required for reproducible
    linking (otherwise a timestamp is embedded).

Reads files only. Uses the standard library only.
"""

import argparse
import fnmatch
import struct
import sys

MACHINES = {"x86": 0x014C, "x64": 0x8664, "arm64": 0xAA64}
# `DependentLoadFlags` written by `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32).
WANT_LOAD_FLAGS = 0x800
IMAGE_DEBUG_TYPE_REPRO = 16
# Case-insensitive patterns for C runtime DLLs that are not allowed as imports.
FORBIDDEN = ["vcruntime*.dll", "msvcp*.dll", "api-ms-win-crt-*.dll", "ucrtbase.dll", "msvcr*.dll", "concrt*.dll"]
# Case-insensitive network modules that are not allowed as imports: the only allowed module is `winhttp.dll` (the
# optional update check; see `crates/update`).
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
    """The file is not a readable PE."""


def _u16(b, o):
    if o < 0 or o + 2 > len(b):
        raise PeError("read out of range")
    return struct.unpack_from("<H", b, o)[0]


def _u32(b, o):
    if o < 0 or o + 4 > len(b):
        raise PeError("read out of range")
    return struct.unpack_from("<I", b, o)[0]


def parse(data):
    """Return (machine, [imported DLLs], [delay-loaded DLLs]) from PE bytes."""
    info = parse_info(data)
    return info["machine"], info["imports"], info["delay"]


def parse_info(data):
    """Todo lo que lee este script de un PE: machine, imports, delay, `load_flags` (`DependentLoadFlags`, o None si el
    load configuration directory does not contain it) and `repro` (whether a REPRO debug entry exists)."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise PeError("not a PE (missing MZ signature)")
    pe = _u32(data, 0x3C)
    if data[pe:pe + 4] != b"PE\0\0":
        raise PeError("not a PE (missing PE signature)")
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
        raise PeError("unknown optional header")
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
                # Between `rsize` and `vsize` (zero-filled in memory), there are no file bytes to read from the
                # next section by accident.
                if delta >= rsize:
                    raise PeError("RVA is outside the section's file data")
                return raw + delta
        raise PeError("RVA is outside all sections")

    def cstr(rva):
        o = rva_to_off(rva)
        end = data.find(b"\0", o)
        if end < 0:
            raise PeError("unterminated string")
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
            raise PeError("too many imports")
    delay = []
    rva, size = directory(13)  # IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT
    if rva and size:
        o = rva_to_off(rva)
        for _ in range(4096):
            attrs = _u32(data, o)
            name_rva = _u32(data, o + 4)
            if attrs == 0 and name_rva == 0:
                break
            # With bit 0 of `attrs` set, addresses are RVAs; otherwise (the old VC6 format) they are virtual addresses.
            # Convert virtual addresses using the image base. A descriptor is NEVER ignored: parse it or fail.
            if not attrs & 1:
                if name_rva < image_base:
                    raise PeError("delay-load descriptor has an address outside the image")
                name_rva -= image_base
            delay.append(cstr(name_rva))
            o += 32
        else:
            raise PeError("too many delay-load imports")
    # Load configuration directory (index 10): `DependentLoadFlags` is at 0x4E (PE32+) or 0x36 (PE32) and is present only if
    # the directory's `Size` field covers it.
    load_flags = None
    rva, size = directory(10)  # IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG
    if rva and size:
        o = rva_to_off(rva)
        field = 0x4E if magic == 0x20B else 0x36
        if _u32(data, o) >= field + 2 and size >= field + 2:
            load_flags = _u16(data, o + field)
    # Debug directory (index 6): 28-byte entries; the type is at +12.
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
            f"{path}: imports a network module other than winhttp.dll: {', '.join(sorted(set(net), key=str.lower))}"
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
    ap.add_argument("--list", action="store_true", help="also print each executable's imports")
    ap.add_argument("exes", nargs="+")
    args = ap.parse_args(argv)
    problems = []
    for path in args.exes:
        problems += check(path, args.machine)
        if args.list:
            try:
                with open(path, "rb") as f:
                    _, imports, delay = parse(f.read())
                print(f"{path}: imports = {', '.join(imports)}" + (f"; delay-loaded = {', '.join(delay)}" if delay else ""))
            except (OSError, PeError, struct.error):
                pass
    if problems:
        for p in problems:
            print(p, file=sys.stderr)
        return 1
    print(
        f"check-pe: OK ({len(args.exes)} executables, {args.machine}, no C runtime imports, "
        "no network modules except winhttp.dll, DependentLoadFlags 0x800, reproducible linking)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
