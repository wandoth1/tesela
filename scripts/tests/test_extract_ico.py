"""Tests for scripts/extract-ico.py using synthetic PEs built here (no real binaries or machine data).

    python -I -m unittest discover -s scripts/tests -p "test_*.py"
"""

import importlib.util
import os
import struct
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("extract_ico", os.path.join(os.path.dirname(HERE), "extract-ico.py"))
extract_ico = importlib.util.module_from_spec(spec)
spec.loader.exec_module(extract_ico)

SECTION_RVA = 0x1000


def make_rsrc(resources, cycle_type=None):
    """A .rsrc section with `resources` = {type: {id: bytes}} (one language). Return the section bytes."""
    buf = bytearray()
    patches = []  # (RVA field position in the data entry, blob position)

    def new_dir(entries):
        off = len(buf)
        buf.extend(struct.pack("<IIHHHH", 0, 0, 0, 0, 0, len(entries)))
        for ident, _ in entries:
            buf.extend(struct.pack("<II", ident, 0))
        return off

    def set_entry(dir_off, index, target):
        struct.pack_into("<I", buf, dir_off + 16 + 8 * index + 4, target)

    blobs = []
    cycle_dirs = []
    root = new_dir([(t, None) for t in sorted(resources)])
    for ti, t in enumerate(sorted(resources)):
        tdir = new_dir([(i, None) for i in sorted(resources[t])])
        set_entry(root, ti, 0x80000000 | tdir)
        for ni, n in enumerate(sorted(resources[t])):
            ndir = new_dir([(1033, None)])
            if t == cycle_type:
                cycle_dirs.append(ndir)
            set_entry(tdir, ni, 0x80000000 | ndir)
            data_entry = len(buf)
            buf.extend(struct.pack("<IIII", 0, len(resources[t][n]), 0, 0))
            set_entry(ndir, 0, data_entry)
            blobs.append((data_entry, resources[t][n]))
    for ndir in cycle_dirs:
        set_entry(ndir, 0, 0x80000000 | ndir)  # ciclo: el directorio de idioma se apunta a si mismo
    for data_entry, blob in blobs:
        struct.pack_into("<I", buf, data_entry, SECTION_RVA + len(buf))
        buf.extend(blob)
        buf.extend(b"\0" * (-len(buf) % 4))
    return bytes(buf)


def make_pe(resources, pe32=False, cycle_type=None):
    sec = make_rsrc(resources, cycle_type) if resources is not None else b"\0" * 16
    sec = sec.ljust(0x200 * ((len(sec) + 0x1FF) // 0x200), b"\0")
    dd_off = 96 if pe32 else 112
    opt_size = dd_off + 16 * 8
    opt = bytearray(opt_size)
    struct.pack_into("<H", opt, 0, 0x10B if pe32 else 0x20B)
    struct.pack_into("<I", opt, dd_off - 4, 16)
    if resources is not None:
        struct.pack_into("<II", opt, dd_off + 8 * 2, SECTION_RVA, len(sec))
    dos = bytearray(0x40)
    dos[:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 0x40)
    coff = struct.pack("<HHIIIHH", 0x8664, 1, 0, 0, 0, opt_size, 0x22)
    raw_off = 0x400
    section = struct.pack("<8sIIIIIIHHI", b".rsrc", len(sec), SECTION_RVA, len(sec), raw_off, 0, 0, 0, 0, 0x40000040)
    header = (bytes(dos) + b"PE\0\0" + coff + bytes(opt) + section).ljust(raw_off, b"\0")
    return header + sec


def group_icon(images):
    """RT_GROUP_ICON for `images` = [(width, height, id, bytes)]."""
    out = struct.pack("<HHH", 0, 1, len(images))
    for w, h, ident, data in images:
        out += struct.pack("<BBBBHHIH", w, h, 0, 0, 1, 32, len(data), ident)
    return out


class ExtractIcoTests(unittest.TestCase):
    def test_extrae_el_ico_con_todas_las_imagenes(self):
        a, b = b"\x89PNG-a" * 5, b"\x89PNG-b" * 9
        pe = make_pe({3: {1: a, 2: b}, 14: {1: group_icon([(16, 16, 1, a), (0, 0, 2, b)])}, 16: {1: b"ver"}})
        ico = extract_ico.extract(pe)
        reserved, kind, count = struct.unpack_from("<HHH", ico, 0)
        self.assertEqual((reserved, kind, count), (0, 1, 2))
        w, h, _c, _r, planes, bits, size, off = struct.unpack_from("<BBBBHHII", ico, 6)
        self.assertEqual((w, h, planes, bits, size, off), (16, 16, 1, 32, len(a), 6 + 32))
        self.assertEqual(ico[off:off + size], a)
        w, h, _c, _r, _p, _b, size, off = struct.unpack_from("<BBBBHHII", ico, 22)
        self.assertEqual((w, h, size), (0, 0, len(b)))
        self.assertEqual(ico[off:off + size], b)

    def test_funciona_con_pe32(self):
        a = b"icon-bytes"
        ico = extract_ico.extract(make_pe({3: {1: a}, 14: {1: group_icon([(32, 32, 1, a)])}}, pe32=True))
        self.assertEqual(ico[-len(a):], a)

    def test_sin_recursos_falla(self):
        with self.assertRaises(extract_ico.PeError):
            extract_ico.extract(make_pe(None))

    def test_sin_icono_falla(self):
        with self.assertRaises(extract_ico.PeError):
            extract_ico.extract(make_pe({16: {1: b"ver"}}))

    def test_falta_una_imagen_del_grupo(self):
        a = b"x" * 8
        pe = make_pe({3: {1: a}, 14: {1: group_icon([(16, 16, 1, a), (32, 32, 2, a)])}})
        with self.assertRaises(extract_ico.PeError):
            extract_ico.extract(pe)

    def test_un_arbol_de_recursos_con_un_ciclo_falla_sin_colgarse(self):
        a = b"x" * 8
        for cycle in (14, 3):
            pe = make_pe({3: {1: a}, 14: {1: group_icon([(16, 16, 1, a)])}}, cycle_type=cycle)
            with self.assertRaises(extract_ico.PeError):
                extract_ico.extract(pe)

    def test_un_pe_truncado_solo_da_pe_error(self):
        a = b"y" * 40
        pe = make_pe({3: {1: a}, 14: {1: group_icon([(16, 16, 1, a)])}})
        self.assertTrue(extract_ico.extract(pe))
        # Through the last data byte (the remainder is section alignment padding).
        end = 0x400 + len(make_rsrc({3: {1: a}, 14: {1: group_icon([(16, 16, 1, a)])}})) - 4
        for cut in list(range(0, end, 17)) + [end - 1]:
            with self.assertRaises(extract_ico.PeError, msg=f"corte en {cut}"):
                extract_ico.extract(pe[:cut])

    def test_un_rva_en_el_relleno_virtual_de_la_seccion_no_se_lee_del_archivo(self):
        # GM-m10: the section is larger in memory (vsize) than in the file (rsize); an RVA in that tail has no file bytes
        # and cannot be translated to an offset (which would read whatever follows).
        a = b"x" * 8
        pe = bytearray(make_pe({3: {1: a}, 14: {1: group_icon([(16, 16, 1, a)])}}))
        sec_hdr = 0x40 + 4 + 20 + (112 + 16 * 8)  # cabecera de la unica seccion (PE32+)
        rsize = struct.unpack_from("<I", pe, sec_hdr + 16)[0]
        struct.pack_into("<I", pe, sec_hdr + 8, rsize + 0x1000)  # vsize mayor que rsize
        pe += b"\0" * 0x1000  # y bytes tras la seccion, para que un desplazamiento sin comprobar caiga DENTRO del archivo
        dd = 0x40 + 4 + 20 + 112
        struct.pack_into("<I", pe, dd + 8 * 2, SECTION_RVA + rsize + 0x10)  # el directorio de recursos, en la cola virtual
        with self.assertRaisesRegex(extract_ico.PeError, "section's file data"):
            extract_ico.extract(bytes(pe))

    def test_no_es_un_pe(self):
        with self.assertRaises(extract_ico.PeError):
            extract_ico.extract(b"nada" * 40)


if __name__ == "__main__":
    unittest.main()
