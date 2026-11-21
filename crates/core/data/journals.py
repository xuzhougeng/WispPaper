"""Builds journals.tsv, the journal metrics Wisp Study carries (`wisp_core::journals`), from ShowJCR's CSVs.

    python3 -I crates/core/data/journals.py <folder with the CSVs> > crates/core/data/journals.tsv

The folder is ShowJCR's `中科院分区表及JCR原始数据文件/` (https://github.com/hitfyd/ShowJCR). A new edition is
a change of the four names below, a run of this script, and of `EDITIONS` in `crates/core/src/journals.rs`.

One row per journal, the editions joined by ISSN, then by name. Columns, tab separated:
names (`|` between), ISSN, eISSN, impact factor, best JCR quartile, its JCR category,
中科院 zone (1–4), 中科院 Top (1/0), 中科院 大类, 新锐 zone, 新锐 Top, 新锐 大类, warning.
"""

import csv
import re
import sys
from pathlib import Path

JCR = "JCR2025-UTF8.csv"
CAS = "FQBJCR2025-UTF8.csv"
XR = "XR2026-UTF8.csv"
WARNING = "GJQKYJMD2025.csv"


def rows(folder, name):
    with open(Path(folder) / name, encoding="utf-8-sig", newline="") as f:
        return list(csv.DictReader(f))


def key(name):
    """The same key `journals::key` makes: lower case, & as and, words of letters and digits, no leading 'the'."""
    words = re.sub(r"[^\w]+", " ", name.lower().replace("&", " and ")).split()
    if words and words[0] == "the":
        words = words[1:]
    return " ".join(words)


def issns(*values):
    found = []
    for value in values:
        for part in re.split(r"[/;, ]+", value or ""):
            part = part.strip().upper()
            if re.fullmatch(r"\d{4}-\d{3}[\dX]", part) and part not in found:
                found.append(part)
    return found


def zone(text):
    m = re.match(r"\s*([1-4])", text or "")
    return m.group(1) if m else ""


def clean(text):
    return re.sub(r"\s+", " ", (text or "").replace("\t", " ")).strip()


def main(folder):
    journals = []
    by_issn = {}
    by_name = {}

    def entry(names, numbers):
        found = next((by_issn[n] for n in numbers if n in by_issn), None)
        if found is None:
            found = next((by_name[key(n)] for n in names if key(n) in by_name), None)
        if found is None:
            found = {"names": [], "issn": "", "eissn": "", "if": "", "q": "", "qcat": "", "cas": "", "castop": "", "cascat": "", "xr": "", "xrtop": "", "xrcat": "", "warn": ""}
            journals.append(found)
        for n in names:
            n = clean(n)
            same = next((i for i, m in enumerate(found["names"]) if key(m) == key(n)), None) if n else None
            if n and key(n) and same is None:
                found["names"].append(n)
            elif same is not None and found["names"][same].isupper() and not n.isupper():
                # The journal's own spelling over JCR's capitals.
                found["names"][same] = n
            if key(n):
                by_name.setdefault(key(n), found)
        for n in numbers:
            by_issn.setdefault(n, found)
        return found

    for r in rows(folder, JCR):
        numbers = issns(r["ISSN"], r["EISSN"])
        j = entry([r["Journal"]], numbers)
        j["issn"] = j["issn"] or (issns(r["ISSN"]) or [""])[0]
        j["eissn"] = j["eissn"] or (issns(r["EISSN"]) or [""])[0]
        impact = r.get("IF(2025)", "").strip()
        j["if"] = impact if re.fullmatch(r"\d+(\.\d+)?", impact) else j["if"]
        # The best of its categories' quartiles.
        best = None
        for i in range(1, 7):
            q, cat = r.get(f"IF Quartile(2025)_{i}", "").strip(), clean(r.get(f"Category_{i}", ""))
            if re.fullmatch(r"Q[1-4]", q) and (best is None or q < best[0]):
                best = (q, cat)
        if best:
            j["q"], j["qcat"] = best

    for r in rows(folder, CAS):
        numbers = issns(r["ISSN/EISSN"])
        j = entry([r["Journal"]], numbers)
        parts = (r["ISSN/EISSN"] or "").split("/")
        j["issn"] = j["issn"] or (issns(parts[0]) or [""])[0]
        j["eissn"] = j["eissn"] or (issns(parts[-1]) or [""])[0] if len(parts) > 1 else j["eissn"]
        j["cas"] = zone(r["大类分区"])
        j["castop"] = "1" if r["Top"].strip() == "是" else "0"
        j["cascat"] = clean(r["大类"])
        if "预警" in r.get("标注", ""):
            j["warn"] = j["warn"] or clean(r["标注"])

    for r in rows(folder, XR):
        numbers = issns(r["ISSN"], r["EISSN"])
        j = entry([r["Journal"], r["刊名"], r["中文刊名"]], numbers)
        j["issn"] = j["issn"] or (issns(r["ISSN"]) or [""])[0]
        j["eissn"] = j["eissn"] or (issns(r["EISSN"]) or [""])[0]
        j["xr"] = zone(r["大类新锐分区"])
        j["xrtop"] = "1" if r["Top"].strip() == "Top" else "0"
        j["xrcat"] = clean(r["大类中文名"])
        if r["预警标记"].strip():
            j["warn"] = j["warn"] or "新锐期刊分区表 Under Review"

    for r in rows(folder, WARNING):
        j = entry([r["Journal"]], [])
        reason = clean(next((v for k, v in r.items() if k.startswith("预警原因")), ""))
        j["warn"] = "国际期刊预警名单 2025" + (f"：{reason}" if reason else "")

    out = csv.writer(sys.stdout, delimiter="\t", lineterminator="\n", quoting=csv.QUOTE_NONE, escapechar=None)
    for j in sorted(journals, key=lambda j: key(j["names"][0]) if j["names"] else ""):
        if not j["names"]:
            continue
        # JCR writes most names in capitals: a name written as the journal writes it comes first, to be shown.
        names = sorted((n.replace("|", " ") for n in j["names"]), key=lambda n: n.isupper())
        out.writerow(["|".join(names), j["issn"], j["eissn"], j["if"], j["q"], j["qcat"], j["cas"], j["castop"], j["cascat"], j["xr"], j["xrtop"], j["xrcat"], j["warn"]])


if __name__ == "__main__":
    main(sys.argv[1])
