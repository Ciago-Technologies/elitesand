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
for name, p in packages.items():
    if name != "ciagomem-crypto":
        assert not {"hmac", "sha2", "blake3"} & {d["name"] for d in p["dependencies"]}

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
for path in [root / "README.md", root / "docs/REVIEW_REMEDIATION.md", spec]:
    doc = path.read_text()
    assert doc.count("```") % 2 == 0, path
    for target in re.findall(r"\]\((<[^>]+>|[^)]+)\)", doc):
        target = target.strip("<>")
        if target.startswith(("http:", "https:", "#")):
            continue
        assert (path.parent / target.split("#", 1)[0]).exists(), (path, target)
assert all((root / "vendor" / p).exists() for p in json.loads((root / "vendor/archive-sha256.json").read_text()))
print("Validated crate DAG, parser/crypto boundaries, opt-in legacy API, source listing, links and vendor inventory.")
print("VERDICT: PASS")
