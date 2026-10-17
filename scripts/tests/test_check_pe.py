"""Tests for scripts/check-pe.py using synthetic PEs built here (no real binaries or machine data).

    python -I -m unittest discover -s scripts/tests -p "test_*.py"
"""

import importlib.util
import os
import struct
import sys
import tempfile
import unittest
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("check_pe", os.path.join(os.path.dirname(HERE), "check-pe.py"))
check_pe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_pe)

SECTION_RVA = 0x1000
FILE_ALIGN = 0x200


def make_pe(machine=0x8664, imports=(), delay=(), pe32=False, delay_old=False, load_flags=0x800, repro=True, vsize_extra=0):
    """A minimal PE: one section with the import (and delay-load) tables and DLL names.

    `load_flags`: `DependentLoadFlags` in the load configuration directory (None = no directory). `repro`: include a
    REPRO debug entry. `vsize_extra`: virtual padding bytes (no file data) after the section data."""
    # Section data: [import descriptors][terminator][delay descriptors][terminator][names]
    n_imp = len(imports)
    n_del = len(delay)
    imp_off = 0
    del_off = 20 * (n_imp + 1)
    names_off = del_off + 32 * (n_del + 1)
    names = b""
    name_rva = {}
    for n in list(imports) + list(delay):
        if n not in name_rva:
            name_rva[n] = SECTION_RVA + names_off + len(names)
            names += n.encode("ascii") + b"\0"
    sec = bytearray()
    for n in imports:
        sec += struct.pack("<IIIII", 0, 0, 0, name_rva[n], 0)
    sec += b"\0" * 20
    image_base = 0x400000 if pe32 else 0x140000000
    for n in delay:
        # `attrs` con el bit 0 = direcciones RVA; sin el (formato antiguo) = direcciones virtuales absolutas.
        # (In PE32+, fields are 32-bit: an absolute address does not fit, so the descriptor is invalid.)
        absolute = image_base + name_rva[n] if pe32 else name_rva[n]
        attrs, name = (0, absolute) if delay_old else (1, name_rva[n])
        sec += struct.pack("<IIIIIIII", attrs, name, 0, 0, 0, 0, 0, 0)
    sec += b"\0" * 32
    sec += names
    # Load configuration directory (with `Size` covering `DependentLoadFlags`) and debug directory.
    sec = bytearray(bytes(sec).ljust((len(sec) + 7) // 8 * 8, b"\0"))
    lc_off = len(sec)
    lc_field = 0x36 if pe32 else 0x4E
    if load_flags is not None:
        lc = bytearray(lc_field + 2 + 6)
        struct.pack_into("<I", lc, 0, len(lc))
        struct.pack_into("<H", lc, lc_field, load_flags)
        sec += lc
    dbg_off = len(sec)
    if repro is not None:
        # Una entrada de tipo CODEVIEW (2) y, si `repro`, otra de tipo REPRO (16).
        for kind in [2] + ([16] if repro else []):
            sec += struct.pack("<IIHHIIII", 0, 0, 0, 0, kind, 0, 0, 0)
    dbg_size = len(sec) - dbg_off
    sec = bytes(sec).ljust(FILE_ALIGN, b"\0")

    opt_magic = 0x10B if pe32 else 0x20B
    dd_off = 96 if pe32 else 112
    opt_size = dd_off + 16 * 8
    opt = bytearray(opt_size)
    struct.pack_into("<H", opt, 0, opt_magic)
    struct.pack_into("<I", opt, dd_off - 4, 16)
    if pe32:
        struct.pack_into("<I", opt, 28, image_base)
    else:
        struct.pack_into("<Q", opt, 24, image_base)
    if n_imp:
        struct.pack_into("<II", opt, dd_off + 8 * 1, SECTION_RVA + imp_off, 20 * (n_imp + 1))
    if n_del:
        struct.pack_into("<II", opt, dd_off + 8 * 13, SECTION_RVA + del_off, 32 * (n_del + 1))
    if load_flags is not None:
        struct.pack_into("<II", opt, dd_off + 8 * 10, SECTION_RVA + lc_off, dbg_off - lc_off)
    if dbg_size:
        struct.pack_into("<II", opt, dd_off + 8 * 6, SECTION_RVA + dbg_off, dbg_size)
    e_lfanew = 0x40
    dos = bytearray(e_lfanew)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, e_lfanew)
    coff = struct.pack("<HHIIIHH", machine, 1, 0, 0, 0, opt_size, 0x22)
    raw_off = 0x400
    section = struct.pack(
        "<8sIIIIIIHHI", b".rdata", len(sec) + vsize_extra, SECTION_RVA, len(sec), raw_off, 0, 0, 0, 0, 0x40000040
    )
    header = bytes(dos) + b"PE\0\0" + coff + bytes(opt) + section
    header = header.ljust(raw_off, b"\0")
    return header + sec


class Temp:
    """Carpeta temporal propia, borrada al salir."""

    def __enter__(self):
        self.path = os.path.join(tempfile.gettempdir(), f"tesela-test-{uuid.uuid4()}")
        os.mkdir(self.path)
        return self

    def write(self, name, data):
        p = os.path.join(self.path, name)
        with open(p, "wb") as f:
            f.write(data)
        return p

    def __exit__(self, *exc):
        for n in os.listdir(self.path):
            os.remove(os.path.join(self.path, n))
        os.rmdir(self.path)


class CheckPe(unittest.TestCase):
    def test_parsea_arquitectura_imports_y_carga_retardada(self):
        m, imp, dly = check_pe.parse(make_pe(0x8664, ["KERNEL32.dll", "USER32.dll"], ["ole32.dll"]))
        self.assertEqual((m, imp, dly), (0x8664, ["KERNEL32.dll", "USER32.dll"], ["ole32.dll"]))
        m, imp, dly = check_pe.parse(make_pe(0xAA64, [], [], pe32=False))
        self.assertEqual((m, imp, dly), (0xAA64, [], []))
        m, imp, _ = check_pe.parse(make_pe(0x014C, ["KERNEL32.dll"], pe32=True))
        self.assertEqual((m, imp), (0x014C, ["KERNEL32.dll"]))

    def test_un_descriptor_diferido_en_formato_antiguo_se_interpreta_y_no_se_ignora(self):
        # PE32 con descriptor antiguo (direcciones virtuales absolutas): se convierte con la base de la imagen.
        m, imp, dly = check_pe.parse(make_pe(machine=0x014C, imports=["KERNEL32.dll"], delay=["VCRUNTIME140.dll"], pe32=True, delay_old=True))
        self.assertEqual(dly, ["VCRUNTIME140.dll"])
        self.assertEqual(check_pe.forbidden(imp + dly), ["VCRUNTIME140.dll"])
        with Temp() as t:
            old = t.write("old.exe", make_pe(machine=0x014C, imports=["KERNEL32.dll"], delay=["MSVCP140.dll"], pe32=True, delay_old=True))
            self.assertEqual(len(check_pe.check(old, "x86")), 1)
            # Un descriptor sin el bit de RVA cuya direccion cae por debajo de la base de la imagen: error, NO se ignora.
            bad = t.write("bad.exe", make_pe(imports=["KERNEL32.dll"], delay=["a.dll"], delay_old=True))
            problems = check_pe.check(bad, "x64")
            self.assertEqual(len(problems), 1)
            self.assertIn("outside the image", problems[0])

    def test_acepta_un_ejecutable_sin_runtime_de_c_y_rechaza_cada_dll_prohibida(self):
        with Temp() as t:
            ok = t.write("ok.exe", make_pe(imports=["KERNEL32.dll", "ntdll.dll", "USER32.dll", "d3d12.dll"]))
            self.assertEqual(check_pe.check(ok, "x64"), [])
            for dll in [
                "VCRUNTIME140.dll",
                "vcruntime140_1.dll",
                "MSVCP140.dll",
                "msvcp140_1.dll",
                "api-ms-win-crt-runtime-l1-1-0.dll",
                "API-MS-WIN-CRT-STDIO-L1-1-0.DLL",
                "ucrtbase.dll",
                "MSVCR120.dll",
            ]:
                bad = t.write("bad.exe", make_pe(imports=["KERNEL32.dll", dll]))
                problems = check_pe.check(bad, "x64")
                self.assertEqual(len(problems), 1, dll)
                self.assertIn("runtime de C", problems[0], dll)
            # Also when it appears only in the delay-load table.
            late = t.write("late.exe", make_pe(imports=["KERNEL32.dll"], delay=["VCRUNTIME140.dll"]))
            self.assertEqual(len(check_pe.check(late, "x64")), 1)
            # Nombres parecidos que NO son del runtime de C.
            fine = t.write("fine.exe", make_pe(imports=["vcruntime.txt.dll.bak", "myvcruntime140.dll", "api-ms-win-core-file-l1-1-0.dll"]))
            self.assertEqual(check_pe.check(fine, "x64"), [])

    def test_el_unico_modulo_de_red_admitido_es_winhttp(self):
        with Temp() as t:
            # winhttp.dll (la comprobacion de actualizaciones) y las DLL normales de la interfaz: bien.
            ok = t.write("ok.exe", make_pe(imports=["KERNEL32.dll", "winhttp.dll", "WINHTTP.DLL", "dxgi.dll", "user32.dll"]))
            self.assertEqual(check_pe.check(ok, "x64"), [])
            for dll in [
                "ws2_32.dll",
                "WS2_32.dll",
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
            ]:
                # Aunque winhttp.dll este, cualquier OTRO modulo de red se rechaza (import normal o retardado).
                for kw in ({"imports": ["KERNEL32.dll", "winhttp.dll", dll]}, {"imports": ["KERNEL32.dll"], "delay": [dll]}):
                    bad = t.write("bad.exe", make_pe(**kw))
                    problems = check_pe.check(bad, "x64")
                    self.assertEqual(len(problems), 1, (dll, kw))
                    self.assertIn("network module", problems[0], dll)
                    self.assertIn(dll, problems[0])
            # Nombres parecidos que NO son modulos de red prohibidos.
            fine = t.write("fine.exe", make_pe(imports=["KERNEL32.dll", "ws2_32.dll.bak", "myws2_32.dll", "api-ms-win-core-file-l1-1-0.dll"]))
            self.assertEqual(check_pe.check(fine, "x64"), [])
            # Y el codigo de salida de main.
            bad = t.write("bad.exe", make_pe(imports=["KERNEL32.dll", "wininet.dll"]))
            self.assertEqual(check_pe.main(["--machine", "x64", ok]), 0)
            self.assertEqual(check_pe.main(["--machine", "x64", bad]), 1)

    def test_exige_dependentloadflags_system32_y_enlace_reproducible(self):
        with Temp() as t:
            ok = t.write("ok.exe", make_pe(imports=["KERNEL32.dll"]))
            self.assertEqual(check_pe.check(ok, "x64"), [])
            info = check_pe.parse_info(make_pe(imports=["KERNEL32.dll"]))
            self.assertEqual((info["load_flags"], info["repro"]), (0x800, True))
            # PE32 (the field is at a different offset).
            pe32 = t.write("pe32.exe", make_pe(machine=0x014C, imports=["KERNEL32.dll"], pe32=True))
            self.assertEqual(check_pe.check(pe32, "x86"), [])
            # Without the load configuration directory, or with other flags (0x1000 = DEFAULT_DIRS, 0 = none).
            for flags in (None, 0, 0x1000, 0x400):
                bad = t.write("bad.exe", make_pe(imports=["KERNEL32.dll"], load_flags=flags))
                problems = check_pe.check(bad, "x64")
                self.assertEqual(len(problems), 1, flags)
                self.assertIn("DependentLoadFlags", problems[0])
            # Without the REPRO entry (with or without a debug directory).
            for repro in (False, None):
                bad = t.write("bad.exe", make_pe(imports=["KERNEL32.dll"], repro=repro))
                problems = check_pe.check(bad, "x64")
                self.assertEqual(len(problems), 1, repro)
                self.assertIn("REPRO", problems[0])
            # Los dos problemas a la vez, y el runtime de C: tres mensajes.
            worse = t.write("worse.exe", make_pe(imports=["VCRUNTIME140.dll"], load_flags=None, repro=False))
            self.assertEqual(len(check_pe.check(worse, "x64")), 3)

    def test_un_rva_en_el_relleno_virtual_de_la_seccion_no_se_lee_del_archivo(self):
        # The section is larger in memory than in the file: an RVA in that tail has no file bytes and must not read
        # whatever follows it (previously this returned an offset outside the section).
        data = bytearray(make_pe(imports=["KERNEL32.dll"], vsize_extra=0x1000))
        # The import descriptor points to a name in the virtual tail: SECTION_RVA + 0x400 (the file section is 0x200 bytes).
        struct.pack_into("<I", data, 0x400 + 12, SECTION_RVA + 0x400)
        with self.assertRaises(check_pe.PeError):
            check_pe.parse(bytes(data))
        with Temp() as t:
            self.assertEqual(len(check_pe.check(t.write("cola.exe", bytes(data)), "x64")), 1)

    def test_la_arquitectura_tiene_que_ser_la_esperada(self):
        with Temp() as t:
            arm = t.write("arm.exe", make_pe(machine=0xAA64, imports=["KERNEL32.dll"]))
            self.assertEqual(check_pe.check(arm, "arm64"), [])
            problems = check_pe.check(arm, "x64")
            self.assertEqual(len(problems), 1)
            self.assertIn("arquitectura", problems[0])
            x86 = t.write("x86.exe", make_pe(machine=0x014C, imports=[]))
            self.assertEqual(len(check_pe.check(x86, "x64")), 1)

    def test_entradas_que_no_son_un_pe_o_estan_truncadas_dan_error_y_no_excepcion(self):
        with Temp() as t:
            for name, data in [
                ("vacio.exe", b""),
                ("texto.exe", b"hola, esto no es un ejecutable"),
                ("mz.exe", b"MZ" + b"\0" * 100),
                ("trunc.exe", make_pe(imports=["KERNEL32.dll"])[:0x60]),
                ("trunc2.exe", make_pe(imports=["KERNEL32.dll"])[:0x420]),
            ]:
                p = t.write(name, data)
                problems = check_pe.check(p, "x64")
                self.assertEqual(len(problems), 1, name)
            self.assertEqual(len(check_pe.check(os.path.join(t.path, "no-existe.exe"), "x64")), 1)

    def test_main_devuelve_codigos_de_salida(self):
        with Temp() as t:
            ok = t.write("ok.exe", make_pe(imports=["KERNEL32.dll"]))
            bad = t.write("bad.exe", make_pe(imports=["VCRUNTIME140.dll"]))
            self.assertEqual(check_pe.main(["--machine", "x64", ok]), 0)
            self.assertEqual(check_pe.main(["--machine", "x64", ok, bad]), 1)
            self.assertEqual(check_pe.main(["--machine", "arm64", ok]), 1)

    def test_lee_un_ejecutable_real_del_sistema(self):
        # The interpreter itself is a real PE: it parses successfully and imports KERNEL32.
        with open(sys.executable, "rb") as f:
            machine, imports, _ = check_pe.parse(f.read())
        self.assertIn(machine, check_pe.MACHINES.values())
        self.assertTrue(any(i.lower() == "kernel32.dll" for i in imports), imports)


if __name__ == "__main__":
    unittest.main()
