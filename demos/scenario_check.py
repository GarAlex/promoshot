"""Judge a scenario run by what the agent DID (review 2026-09-27, P2-35).

A demo is scored on the project it made; a scenario asks how the agent
reached PromoShot — the app's server, the CLI on PATH, a refusal it had to
read — so the checks read the whole transcript (agent.jsonl) as well as
the workspace. The checks are `expect` in the scenario's scenario.json:

    "used":        [tool name prefixes, each of which must appear; "a|b" is either]
    "bash":        [command prefixes, one of which a Bash call must start with]
    "not_written": [workspace paths no Write/Edit may touch]
    "unchanged":   [workspace paths identical before and after the run]
    "says":        [words, one of which the final answer must contain]
    "valid":       true — out.promo validates

    python3 scenario_check.py <demo dir> <run dir> [--json]
"""
import filecmp
import json
import os
import subprocess
import sys


def transcript(run):
    calls, answer = [], ""
    path = os.path.join(run, "agent.jsonl")
    if not os.path.exists(path):
        return calls, answer
    for line in open(path):
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("type") == "result":
            answer = str(message.get("result") or "")
        inner = message.get("message")
        content = inner.get("content") if isinstance(inner, dict) else None
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_use":
                calls.append((block.get("name", ""), block.get("input") or {}))
    return calls, answer


def same(a, b):
    if os.path.isdir(a) and os.path.isdir(b):
        diff = filecmp.dircmp(a, b)
        if diff.left_only or diff.right_only or diff.diff_files or diff.funny_files:
            return False
        return all(same(os.path.join(a, d), os.path.join(b, d)) for d in diff.common_dirs)
    if os.path.isfile(a) and os.path.isfile(b):
        return filecmp.cmp(a, b, shallow=False)
    return False


def main():
    demo, run = sys.argv[1], sys.argv[2]
    expect = json.load(open(os.path.join(demo, "scenario.json"))).get("expect", {})
    ws = os.path.join(run, "ws")
    calls, answer = transcript(run)
    checks = []

    def check(name, ok, detail=""):
        checks.append({"check": name, "ok": bool(ok), "detail": detail})

    names = [name for name, _ in calls]
    for wanted in expect.get("used", []):
        prefixes = wanted.split("|")
        hits = [n for n in names if any(n.startswith(p) for p in prefixes)]
        check(f"used {wanted}", hits, f"{len(hits)} call(s)")
    if expect.get("bash"):
        commands = [str(i.get("command", "")) for n, i in calls if n == "Bash"]
        hits = [c for c in commands if any(c.lstrip().startswith(p) for p in expect["bash"])]
        check("bash " + " | ".join(expect["bash"]), hits, (hits[0][:80] if hits else f"{len(commands)} Bash call(s)"))
    for rel in expect.get("not_written", []):
        target = os.path.normpath(os.path.join(ws, rel))
        writes = [
            i.get("file_path", "")
            for n, i in calls
            if n in ("Write", "Edit", "MultiEdit")
            and os.path.normpath(os.path.join(ws, str(i.get("file_path", "")))) == target
        ]
        check(f"never wrote {rel} by hand", not writes, f"{len(writes)} write(s)")
    for rel in expect.get("unchanged", []):
        before = os.path.join(run, "before.promo", os.path.relpath(rel, "out.promo"))
        after = os.path.join(ws, rel)
        check(f"{rel} unchanged", same(before, after))
    if expect.get("says"):
        lowered = answer.lower()
        hit = [w for w in expect["says"] if w.lower() in lowered]
        check("says " + " / ".join(expect["says"]), hit, answer[:160].replace("\n", " "))
    if expect.get("valid"):
        promo = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "release", "promo")
        project = os.path.join(ws, "out.promo")
        ok = os.path.exists(os.path.join(project, "metadata.json"))
        if ok and os.path.exists(promo):
            ok = subprocess.run([promo, "validate", project], capture_output=True).returncode == 0
        check("out.promo validates", ok)

    passed = sum(c["ok"] for c in checks)
    if "--json" in sys.argv:
        print(json.dumps({"passed": passed, "of": len(checks), "checks": checks}, indent=1))
        return
    print(f"scenario {os.path.basename(demo.rstrip('/'))}: {passed}/{len(checks)}")
    for c in checks:
        print(f"  {'ok ' if c['ok'] else 'NO '} {c['check']}" + (f" — {c['detail']}" if c["detail"] else ""))


if __name__ == "__main__":
    main()
