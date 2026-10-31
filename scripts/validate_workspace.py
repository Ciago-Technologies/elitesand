#!/usr/bin/env python3
"""Check dependency boundaries, review coverage, links and the embedded core listing."""
import json
import re
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--offline", "--no-deps", "--format-version", "1",
    "--manifest-path", str(root / "Cargo.toml"),
], text=True))
packages = {p["name"]: p for p in metadata["packages"]}
graph = {n: {d["name"] for d in p["dependencies"] if d["name"] in packages}
         for n, p in packages.items()}
assert not packages["ciagomem-ql"]["dependencies"], "parser reaches storage or crypto"
assert graph["ciagomem-engine"] == {"ciagomem-core", "ciagomem-ql"}
engine_deps = {d["name"] for d in packages["ciagomem-engine"]["dependencies"]}
assert {"roaring", "flatbuffers", "wide"} <= engine_deps
assert all("ciagomem-engine" not in deps for n,deps in graph.items() if n != "ciagomem-engine")
for name, p in packages.items():
    if name != "ciagomem-crypto":
        assert not {"hmac", "sha2", "blake3", "ed25519-dalek", "curve25519-dalek"} & {d["name"] for d in p["dependencies"]}

def visit(node, stack):
    assert node not in stack, f"dependency cycle: {stack} -> {node}"
    for child in graph[node]:
        visit(child, stack + [node])

for node in graph:
    visit(node, [])
core = (root / "ciagomem-core/src/lib.rs").read_text()
assert '#[cfg(feature = "legacy-audit")]\npub mod legacy;' in core
assert "legacy-audit" not in packages["ciagomem-core"]["features"].get("default", [])
spec = root / "MASTER_SPECIFICATION.md"
text = spec.read_text()
match = re.search(r"<!-- BEGIN REVIEWED CORE SOURCE -->\s*```rust\n(.*?)\n```\s*<!-- END REVIEWED CORE SOURCE -->", text, re.S)
assert match, "missing embedded core listing"
listing = match.group(1)
assert listing == core.rstrip(), "specification listing differs from core library"
for path in [root / "README.md", *sorted((root / "docs").glob("*.md")), spec]:
    doc = path.read_text()
    assert doc.count("```") % 2 == 0, path
    for target in re.findall(r"\]\((<[^>]+>|[^)]+)\)", doc):
        target = target.strip("<>")
        if target.startswith(("http:", "https:", "#")):
            continue
        assert (path.parent / target.split("#", 1)[0]).exists(), (path, target)
assert all((root / "vendor" / p).exists() for p in json.loads((root / "vendor/archive-sha256.json").read_text()))
ledger=(root / "docs/R5_REMEDIATION.md").read_text()
required={"M1","D1","W1","I1","L1","T1","T2","T3","F1","F2","F3","F4","F5","P1","P2","P3","P4","Q1","Q2","Q3","Q4","Q5","Q6","X1","X2","A1","A2","A3","A4","O1","V1","S1","L2","C1","C2","C3","B1","G1","G2"}
assert required <= set(re.findall(r"R5-([A-Z]\d+)",ledger)), "R5 finding lacks disposition"
r6=(root / "docs/R6_REMEDIATION.md").read_text()
assert {f"R6-N{i}" for i in range(1,8)} <= set(re.findall(r"R6-N\d+",r6)), "R6 finding lacks disposition"
slot=(root / "ciagomem-core/src/slot.rs").read_text()
assert "pub durable_lsn:" not in slot and "pub fenced:" not in slot
r7=(root / "docs/R7_REMEDIATION.md").read_text()
assert {f"R7-{i}" for i in range(1,7)} <= set(re.findall(r"R7-\d+",r7)), "R7 finding lacks disposition"
r8=(root / "docs/R8_REMEDIATION.md").read_text()
assert {f"R8-{i}" for i in range(1,6)} <= set(re.findall(r"R8-\d+",r8)), "R8 finding lacks disposition"
assert sorted(p.name for p in root.glob("*policy.json"))==["policy.json"], "keep one engineering policy.json"
cleanup=json.loads((root / "docs/cleanup-manifest.json").read_text())
import hashlib
for name,digest in cleanup["preserved_source_files"].items():
    assert hashlib.sha256((root/name).read_bytes()).hexdigest()==digest, name
for name in cleanup["removed_archives"]+cleanup["removed_directories"]:
    assert not (root.parent/name).exists(), f"obsolete artifact reappeared: {name}"
r9=(root / "docs/R9_REMEDIATION.md").read_text()
assert {"R9-1","R9-2"} <= set(re.findall(r"R9-\d+",r9)), "R9 finding lacks disposition"
assert all(f"pub(crate) fn {name}" in slot for name in ["publish_durable","bind_log","unbind_log"]), "raw publication must stay crate-private"
r10 = (root / "docs/R10_REMEDIATION.md").read_text()
assert {f"R10-{i}" for i in range(1, 5)} <= set(re.findall(r"R10-\d+", r10))
evidence = json.loads((root / "docs/r10-evidence.json").read_text())
for name, digests in evidence["files"].items():
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digests["normalized_sha256"], name
r11 = (root / "docs/R11_REMEDIATION.md").read_text()
assert {f"R11-{i}" for i in range(1, 5)} <= set(re.findall(r"R11-\d+", r11))
assert "pub(crate) fn header(" in slot and "pub(crate) fn payload(" in slot
r11_evidence = json.loads((root / "docs/r11-evidence.json").read_text())
for name, digest in r11_evidence["files"].items():
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
workflow = (root / ".github/workflows/rust.yml").read_text()
for command in ["cargo fmt --all -- --check", "python3 scripts/validate_workspace.py",
                "cargo test --release --offline --workspace -- --nocapture",
                "cargo test --release --offline --workspace --all-features -- --nocapture",
                "cargo clippy --offline --workspace --all-targets --all-features",
                "python3 scripts/check_mesh_progress.py"]:
    assert command in workflow, ("missing CI gate", command)
subprocess.run(["python3", str(root / "scripts/validate_spec_contract.py")], check=True)
print("Validated crate DAG, parser/crypto boundaries, opt-in legacy API, source listing, links and vendor inventory.")
policy = json.loads((root / "policy.json").read_text())
for path in root.rglob("*"):
    relative = path.relative_to(root)
    if any(part in {".git", "target", "vendor"} for part in relative.parts):
        continue
    assert not any(old in str(relative).lower() for old in policy["naming"]["forbidden_identifiers"]), relative
    if not path.is_file() or relative == Path("policy.json"):
        continue
    try:
        content = path.read_bytes().decode("utf-8").lower()
    except UnicodeDecodeError:
        continue
    assert not any(old in content for old in policy["naming"]["forbidden_identifiers"]), relative
print("Validated canonical Ciagomem/CiaQL names in owned paths and content.")
print("VERDICT: PASS")
