#!/usr/bin/env python3
"""Feature map: which user-facing features each function serves (docs/features.json).

  featuremap.py check            every non-test function is in a feature, every listed one exists
  featuremap.py render           regenerate docs/features.md from docs/features.json
  featuremap.py affected [RANGE] features touched by a diff (default: uncommitted changes; e.g. HEAD~1..HEAD)
"""
import collections, json, os, subprocess, sys

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()
TOOL = os.path.join(ROOT, "tools", "featuremap")
MAP = os.path.join(ROOT, "docs", "features.json")
OUT = os.path.join(TOOL, "target", "inventory")


def inventory(src_root=None):
    subprocess.run(["cargo", "build", "-q", "--release", "--manifest-path", os.path.join(TOOL, "Cargo.toml")], check=True)
    os.makedirs(OUT, exist_ok=True)
    subprocess.run([os.path.join(TOOL, "target", "release", "featuremap"), src_root or os.path.join(ROOT, "src"), OUT], check=True, stderr=subprocess.DEVNULL)
    fns = {}
    for line in open(os.path.join(OUT, "fns.tsv")):
        if line.strip():
            id_, file, start, end, test, _trait, *_ = line.rstrip("\n").split("\t")
            fns[id_] = dict(file=file, start=int(start), end=int(end), test=test == "1" or "testutil" in file or "testkit" in file)
    return fns


def load():
    m = json.load(open(MAP))
    feats = {f["id"]: (a, g, f) for a in m["areas"] for g in a["groups"] for f in g["features"]}
    return m, feats


def users(feats):
    by_fn = collections.defaultdict(set)
    for fid, (_, _, f) in feats.items():
        for fn in f["fns"]:
            by_fn[fn].add(fid)
    return by_fn


def check():
    fns, (_, feats) = inventory(), load()
    by_fn = users(feats)
    missing = sorted(i for i, f in fns.items() if not f["test"] and i not in by_fn)
    gone = sorted(fn for fn in by_fn if fn not in fns)
    bad_platform = sorted({p for _, _, f in feats.values() for p in f.get("platform", []) if p not in feats})
    for i in missing:
        print(f"not in any feature: {i} ({fns[i]['file']}:{fns[i]['start']})")
    for i in gone:
        print(f"listed but gone: {i} (in {', '.join(sorted(by_fn[i]))})")
    for p in bad_platform:
        print(f"unknown platform feature: {p}")
    print(f"{len(feats)} features, {sum(1 for f in fns.values() if not f['test'])} functions, {len(missing)} unmapped, {len(gone)} stale")
    return 1 if missing or gone or bad_platform else 0


def render():
    m, feats = load()
    fns = inventory()
    by_fn = users(feats)
    dependents = collections.defaultdict(list)
    for fid, (_, _, f) in feats.items():
        for p in f.get("platform", []):
            dependents[p].append(fid)
    loc = lambda fn: f"{fns[fn]['file']}:{fns[fn]['start']}" if fn in fns else "?"
    short = lambda fn: fn.removeprefix("crate::")
    out = ["# Glasir feature map", "",
           "Generated from `docs/features.json` by `tools/featuremap/featuremap.py render`; edit the JSON, not this file.",
           "Every non-test function belongs to at least one feature (`featuremap.py check`). "
           "`featuremap.py affected HEAD~1..HEAD` lists the features a change touches.", ""]
    out += ["## Contents", ""]
    for a in m["areas"]:
        n = sum(len(g["features"]) for g in a["groups"])
        out.append(f"- [{a['name']}](#{a['area']}) ({n} features)")
    out += ["", "## Most shared functions", "", "Changing one of these means re-reviewing every feature listed for it in `features.json`.", "",
            "| Function | Features |", "|---|---|"]
    for n, fn in sorted(((len(v), k) for k, v in by_fn.items()), reverse=True)[:25]:
        out.append(f"| `{short(fn)}` ({loc(fn)}) | {n} |")
    for a in m["areas"]:
        out += ["", f'<a id="{a["area"]}"></a>', f"## {a['name']}"]
        for g in a["groups"]:
            out += ["", f"### {g['name']}"]
            for f in g["features"]:
                out += ["", f"#### {f['name']} `{f['id']}`"]
                if f.get("entry"):
                    out.append("- **Entry:** " + "; ".join(f["entry"]))
                if f["id"].startswith("platform."):
                    out.append(f"- **Used by:** {len(dependents[f['id']])} features")
                if f.get("platform"):
                    out.append("- **Platform:** " + ", ".join(f"`{p}`" for p in f["platform"]))
                own = [fn for fn in f["fns"] if len(by_fn[fn]) == 1]
                shared = [fn for fn in f["fns"] if len(by_fn[fn]) > 1]
                if own:
                    out.append("- **Own code:** " + ", ".join(f"`{short(fn)}`" for fn in own))
                if shared:
                    out.append("- **Shared:** " + ", ".join(f"`{short(fn)}` ({len(by_fn[fn])})" for fn in shared))
    open(os.path.join(ROOT, "docs", "features.md"), "w").write("\n".join(out) + "\n")
    print("wrote docs/features.md")


def affected(rng=None):
    args = ["git", "diff", "-U0", "--no-color", "--no-ext-diff"] + ([rng] if rng else []) + ["--", "src"]
    diff = subprocess.run(args, capture_output=True, text=True, cwd=ROOT, check=True).stdout
    fns, (_, feats) = inventory(), load()
    by_fn = users(feats)
    hit, file = set(), None
    for line in diff.splitlines():
        if line.startswith("+++ "):
            file = line[6:] if line.startswith("+++ b/") else None
        elif line.startswith("@@") and file:
            new = line.split("+")[1].split(" ")[0]
            start, _, count = new.partition(",")
            start, count = int(start), int(count or 1)
            end = start + max(count, 1) - 1
            hit |= {i for i, f in fns.items() if f["file"] == file and f["start"] <= end and f["end"] >= start and not f["test"]}
    direct = collections.defaultdict(set)
    for fn in hit:
        for fid in by_fn.get(fn, ()):
            direct[fid].add(fn)
    platform = {fid for fid in direct if fid.startswith("platform.")}
    through = sorted({fid for fid, (_, _, f) in feats.items() for p in f.get("platform", []) if p in platform} - set(direct))
    for fid in sorted(direct):
        print(f"{fid}: {feats[fid][2]['name']}  <- {', '.join(sorted(fn.removeprefix('crate::') for fn in direct[fid]))}")
    if through:
        print(f"\nvia changed platform code ({', '.join(sorted(platform))}): {len(through)} more features")
        print("  " + ", ".join(through))
    unmapped = sorted(fn for fn in hit if fn not in by_fn)
    if unmapped:
        print("\nchanged functions in no feature yet (add them to docs/features.json):")
        print("\n".join(f"  {fn}" for fn in unmapped))


if __name__ == "__main__":
    cmd, rest = (sys.argv[1] if len(sys.argv) > 1 else "check"), sys.argv[2:]
    sys.exit({"check": check, "render": render, "affected": lambda: affected(*rest)}[cmd]() or 0)
