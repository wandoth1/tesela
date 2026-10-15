#!/usr/bin/env python3
"""Resume los JSON de spike/out en tablas Markdown (uso local; no publiques datos del equipo).

Uso (desde spike/):  python scripts/summarize.py ui|enum|trash|delete
Solo lee JSON; no ejecuta nada del spike.
"""
import glob
import json
import os
import statistics as st
import sys

MIB = 1048576.0
GIB = 1073741824.0


def load(p):
    with open(p, encoding="utf-8") as f:
        return json.load(f)


def ui():
    rows = []
    for p in sorted(glob.glob("out/ui/*.json")):
        d = load(p)
        name = os.path.basename(p)[:-5]
        mem = {m["momento"]: m for m in d["memoria"]}
        f = d["fases"]
        c = f["continuous"]
        h = f["hover"]
        gpu = d["gpu"] or {}
        adapter = gpu.get("adaptador") or gpu.get("renderer") or "?"
        be = gpu.get("backend_wgpu", "GL")
        rows.append(
            (
                name,
                d["backend"],
                f"{adapter} [{be}]",
                d["n_celdas_dibujadas"],
                d["exe_bytes"] / MIB,
                mem["idle_final"]["working_set_mib"],
                mem["idle_final"]["private_bytes_mib"],
                c["cpu_eframe_por_frame"]["p50_ms"],
                c["cpu_eframe_por_frame"]["p95_ms"],
                c["cpu_eframe_por_frame"]["p99_ms"],
                c["intervalo_entre_frames"]["p50_ms"],
                c["fps_medio"],
                h["cpu_eframe_por_frame"]["p50_ms"],
                h["cpu_eframe_por_frame"]["p99_ms"],
                h["fps_medio"],
                d["frames_en_idle"],
            )
        )
    print(
        "| ejecución | backend | adaptador | celdas | exe MiB | WS reposo MiB | private reposo MiB | cont. CPU p50 ms | p95 | p99 | cont. intervalo p50 ms | cont. fps | hover CPU p50 ms | hover p99 | hover fps | frames en reposo |"
    )
    print("|" + "---|" * 16)
    for r in rows:
        print(
            "| {} | {} | {} | {} | {:.1f} | {:.0f} | {:.0f} | {:.1f} | {:.1f} | {:.1f} | {:.1f} | {:.0f} | {:.1f} | {:.1f} | {:.0f} | {} |".format(*r)
        )


def enum():
    runs = []
    for p in sorted(glob.glob("out/enum_*_r[0-9].json")):
        d = load(p)["runs"][0]
        runs.append((os.path.basename(p), d))
    print("| ejecución | API | s | archivos | dirs | alloc GiB (sin dedup) | EOF GiB | errores dir (código: n) |")
    print("|---|---|---|---|---|---|---|---|")
    for n, d in runs:
        print(
            f"| {n[:-5]} | {d['api']} | {d['segundos']:.2f} | {d['archivos']} | {d['directorios']} | "
            f"{d['alloc_sum_archivos']/GIB:.2f} | {d['eof_sum_archivos']/GIB:.2f} | {d['errores_dir_por_codigo_win32']} |"
        )
    print()
    for api in ("extd", "find", "find-alloc"):
        v = [d["segundos"] for n, d in runs if d["api"] == api]
        if v:
            print(f"- {api}: n={len(v)} media={st.mean(v):.2f}s min={min(v):.2f}s max={max(v):.2f}s")
    print()
    print("| hilos (extd) | s |")
    print("|---|---|")
    for p in sorted(glob.glob("out/enum_extd_t*.json"), key=lambda x: int(x.split("_t")[1].split(".")[0])):
        d = load(p)["runs"][0]
        print(f"| {d['threads']} | {d['segundos']:.2f} |")


def trash():
    for p in sorted(glob.glob("out/trash*.json")):
        print("##", p)
        print(json.dumps(load(p), indent=1, ensure_ascii=False))


def delete():
    d = load("out/delete_speed.json")["speed"]
    print("| variante | s por ejecución | media s | archivos/s (media) | fallos/restos |")
    print("|---|---|---|---|---|")
    for v in d["variantes"]:
        print(
            f"| {v['variante']} | {', '.join(f'{x:.2f}' for x in v['segundos_por_ejecucion'])} | {v['media_s']:.2f} | "
            f"{v['archivos_por_s_media']:.0f} | {v['fallos_o_restos_totales']} |"
        )


if __name__ == "__main__":
    {"ui": ui, "enum": enum, "trash": trash, "delete": delete}[sys.argv[1]]()
