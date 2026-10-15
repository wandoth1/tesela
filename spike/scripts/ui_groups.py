#!/usr/bin/env python3
"""Agrupa las ejecuciones de out/ui por configuración (quita el sufijo -rN) y muestra rangos. Uso (desde spike/): python scripts/ui_groups.py"""
import collections
import glob
import json
import re


def rng(v, f="{:.1f}"):
    lo, hi = min(v), max(v)
    return f.format(lo) if abs(hi - lo) < 1e-9 else f.format(lo) + "–" + f.format(hi)


groups = collections.defaultdict(list)
for p in sorted(glob.glob("out/ui/*.json")):
    name = re.split(r"[\/]", p)[-1][:-5]
    groups[re.sub(r"-r\d+$", "", name)].append(json.load(open(p, encoding="utf-8")))

print("| config | n | exe MiB | WS MiB | **private WS MiB** | private bytes MiB | cont. CPU p50 | p95 | p99 | hover p50 | hover p99 | fps cont |")
print("|" + "---|" * 12)
for g, ds in groups.items():
    def idle(k):
        return [[m for m in d["memoria"] if m["momento"] == "idle_final"][0][k] for d in ds]
    def c(k, ph="continuous"):
        return [d["fases"][ph]["cpu_eframe_por_frame"][k] for d in ds]
    fps = [d["fases"]["continuous"]["fps_medio"] for d in ds]
    print(
        f"| {g} | {len(ds)} | {ds[0]['exe_bytes']/2**20:.2f} | {rng(idle('working_set_mib'), '{:.0f}')} | **{rng(idle('private_working_set_mib'), '{:.0f}')}** | "
        f"{rng(idle('private_bytes_mib'), '{:.0f}')} | {rng(c('p50_ms'))} | {rng(c('p95_ms'))} | {rng(c('p99_ms'))} | {rng(c('p50_ms', 'hover'))} | {rng(c('p99_ms', 'hover'))} | {rng(fps, '{:.0f}')} |"
    )
