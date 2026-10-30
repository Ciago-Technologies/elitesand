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
assert not packages["coagomem-ql"]["dependencies"], "parser reaches storage or crypto"
assert graph["ciagomem-engine"] == {"coagomem-core", "coagomem-ql"}
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
core = (root / "coagomem-core/src/lib.rs").read_text()
assert '#[cfg(feature = "legacy-audit")]\npub mod legacy;' in core
assert "legacy-audit" not in packages["coagomem-core"]["features"].get("default", [])
spec = root.parent / "ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md"
text = spec.read_text()
listing = text.split("<!-- BEGIN REVIEWED CORE SOURCE -->\n```rust\n", 1)[1].split("\n```\n<!-- END REVIEWED CORE SOURCE -->", 1)[0]
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
print("Validated crate DAG, parser/crypto boundaries, opt-in legacy API, source listing, links and vendor inventory.")
print("VERDICT: PASS")
