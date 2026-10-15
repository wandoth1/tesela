#!/usr/bin/env python3
"""Redacta datos personales en un archivo de resultados. Uso LOCAL: los resultados NUNCA se publican (contienen
datos del equipo: adaptador, driver, build, tamaños de volumen...); la redacción es solo defensiva.

Uso: redact.py entrada salida

Qué hace (todo sin distinguir mayúsculas):
  * SID de usuario (S-1-5-21-...)                                   -> S-1-5-21-REDACTADO
  * C:\\Users\\<nombre>  (nombre de usuario de Windows)               -> C:\\Users\\<usuario>
  * la ruta del repositorio local (la carpeta que contiene spike/)    -> <repo>
  * JSON: en `toplevel_por_alloc` solo se conservan como claves los nombres de una LISTA BLANCA de carpetas de
    sistema; el resto pasa a `<carpeta-N>` (N = posición por tamaño). Las rutas de `muestras_error` solo se
    conservan bajo raíces de la lista blanca (y, en `Users`, solo hasta el nombre de usuario); lo demás pasa a
    `<ruta-redactada>`.
Después de redactar, `collect-results.sh` comprueba que no quede ninguna palabra de la lista negra.
"""
import json
import os
import re
import sys

WHITELIST = {
    "windows", "program files", "program files (x86)", "programdata", "$recycle.bin", "users", "<raíz>",
    "pagefile.sys", "hiberfil.sys", "swapfile.sys", "system volume information",
}

SEP = r"(?:\\\\|\\|/)"


def text_redact(s: str) -> str:
    s = re.sub(r"S-1-5-21-\d+-\d+-\d+-\d+", "S-1-5-21-REDACTADO", s, flags=re.I)
    s = re.sub(r"(C:" + SEP + r"Users" + SEP + r")[^\\/\"]+", r"\1<usuario>", s, flags=re.I)
    # Ruta del repositorio local, calculada (sin nombres fijos): <repo> = carpeta que contiene spike/.
    repo = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
    parts = [re.escape(x) for x in re.split(r"[\\/]+", repo) if x]
    s = re.sub(parts[0] + SEP + SEP.join(parts[1:]), "<repo>", s, flags=re.I)
    return s


def redact_path(p: str) -> str:
    """Conserva solo rutas bajo raíces de la lista blanca; bajo Users, solo hasta el nombre de usuario."""
    q = p
    if q.startswith("\\\\?\\"):
        q = q[4:]
    parts = [x for x in re.split(r"[\\/]+", q) if x]
    if len(parts) < 2 or not re.fullmatch(r"[A-Za-z]:", parts[0]):
        return "<ruta-redactada>"
    root = parts[1].lower()
    if root not in WHITELIST:
        return "<ruta-redactada>"
    if root == "users":
        return parts[0] + "\\Users\\<usuario>\\<ruta-redactada>"
    return p


def walk(o):
    if isinstance(o, dict):
        out = {}
        for k, v in o.items():
            if k == "toplevel_por_alloc" and isinstance(v, dict):
                ranked = sorted(v.items(), key=lambda kv: -(kv[1].get("alloc", 0) if isinstance(kv[1], dict) else 0))
                new = {}
                n = 0
                for name, val in ranked:
                    if name.lower() in WHITELIST:
                        new[name] = val
                    else:
                        n += 1
                        new[f"<carpeta-{n}>"] = val
                out[k] = new
            elif k in ("muestras_error", "error_samples") and isinstance(v, list):
                out[k] = [[it[0], redact_path(it[1])] if isinstance(it, list) and len(it) == 2 else it for it in v]
            else:
                out[k] = walk(v)
        return out
    if isinstance(o, list):
        return [walk(x) for x in o]
    return o


def main():
    src, dst = sys.argv[1], sys.argv[2]
    s = open(src, encoding="utf-8", errors="replace", newline="").read()
    if src.lower().endswith(".json"):
        try:
            s = json.dumps(walk(json.loads(s)), indent=2, ensure_ascii=False) + "\n"
        except json.JSONDecodeError:
            pass
    s = text_redact(s)
    open(dst, "w", encoding="utf-8", newline="").write(s)


if __name__ == "__main__":
    main()
