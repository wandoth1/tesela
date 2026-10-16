"""Pruebas de scripts/check-pe.py con PE sintéticos construidos aquí (sin binarios reales ni datos del equipo).

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


def make_pe(machine=0x8664, imports=(), delay=(), pe32=False, delay_old=False):
    """Un PE mínimo: una sección con la tabla de imports (y la de carga retardada) y los nombres de las DLL."""
    # Datos de la sección: [descriptores de import][terminador][descriptores de delay][terminador][nombres]
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
        # (En PE32+ los campos son de 32 bits: una direccion absoluta no cabe, y el descriptor es inválido.)
        absolute = image_base + name_rva[n] if pe32 else name_rva[n]
        attrs, name = (0, absolute) if delay_old else (1, name_rva[n])
        sec += struct.pack("<IIIIIIII", attrs, name, 0, 0, 0, 0, 0, 0)
    sec += b"\0" * 32
    sec += names
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
    e_lfanew = 0x40
    dos = bytearray(e_lfanew)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, e_lfanew)
    coff = struct.pack("<HHIIIHH", machine, 1, 0, 0, 0, opt_size, 0x22)
    raw_off = 0x400
    section = struct.pack("<8sIIIIIIHHI", b".rdata", len(sec), SECTION_RVA, len(sec), raw_off, 0, 0, 0, 0, 0x40000040)
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
            self.assertIn("fuera de la imagen", problems[0])

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
            # También si solo aparece en la carga retardada.
            late = t.write("late.exe", make_pe(imports=["KERNEL32.dll"], delay=["VCRUNTIME140.dll"]))
            self.assertEqual(len(check_pe.check(late, "x64")), 1)
            # Nombres parecidos que NO son del runtime de C.
            fine = t.write("fine.exe", make_pe(imports=["vcruntime.txt.dll.bak", "myvcruntime140.dll", "api-ms-win-core-file-l1-1-0.dll"]))
            self.assertEqual(check_pe.check(fine, "x64"), [])

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
        # El propio intérprete es un PE real: se parsea y trae KERNEL32.
        with open(sys.executable, "rb") as f:
            machine, imports, _ = check_pe.parse(f.read())
        self.assertIn(machine, check_pe.MACHINES.values())
        self.assertTrue(any(i.lower() == "kernel32.dll" for i in imports), imports)


if __name__ == "__main__":
    unittest.main()
