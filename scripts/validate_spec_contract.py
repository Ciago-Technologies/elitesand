#!/usr/bin/env python3
"""Check the target specification's layouts and mirrored text, not runtime features."""
import json
import re
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HEADING = "## 14. Application schemas, collections, array storage and language clients"


def check_layout(layout):
    header = layout["slot_header"]
    used = header["fields"] * header["field_bytes"]
    aligned = (used + header["alignment"] - 1) // header["alignment"] * header["alignment"]
    assert header["size"] == aligned == 128
    for name in ("slot_handle_bits", "tenant_word_bits", "reserved_word_bits"):
        assert sum(layout[name].values()) == 64, name
    assert layout["class_bytes"] == [4096 << c for c in range(5)]
    assert layout["reserved_classes"] == list(range(5, 16))
    for prefix in ("wire_header", "request_prefix", "response_prefix", "batch_header",
                   "result_ledger", "export_page_header"):
        assert sum(layout[prefix + "_field_bytes"]) == layout[prefix + "_bytes"], prefix
    assert struct.calcsize("<HBBHIII") == layout["wire_header_bytes"] == 18
    assert struct.calcsize("<4sHHIHHIIQQQQII") == layout["document_prefix_bytes"] == 64
    assert layout["extent_header_bytes"] == 128
    assert layout["wal_envelope_bytes"] == 32
    assert layout["operation_prefix_bytes"] == 8
    assert 32 + 88 + 8 + 8 == 136  # Envelope + batch + one i64 operation, before ledger.
    assert 4096 - 128 - 64 == 3904
    assert 65536 - 128 - 64 == 65344
    assert 4096 - layout["export_page_header_bytes"] == 4032
    assert 5 * 14 * 2 == 140  # MiB reserved by one HugePage per class/core.


def check_offset_table(text, start, end, total, initial=0):
    part = text.split(start, 1)[1].split(end, 1)[0]
    rows = re.findall(r"^\| (\d+) \| (\d+) \|", part, re.M)
    assert rows, start
    offset = initial
    for pos, size in rows:
        assert int(pos) == offset, (start, pos, offset)
        offset += int(size)
    assert offset == total, (start, offset, total)


def check_grammar(grammar):
    # Remove terminals and ISO-style prose terminals before checking rule references.
    stripped = re.sub(r'''"[^"]*"|'[^']*'|\?[^?]*\?''', "", grammar)
    productions = re.findall(r"^([a-z_][a-z_0-9]*)\s*=", stripped, re.M)
    assert len(productions) == len(set(productions)), "duplicate EBNF rule"
    identifiers = set(re.findall(r"\b[a-z_][a-z_0-9]*\b", stripped))
    assert identifiers <= set(productions), identifiers - set(productions)
    assert stripped.count(";") == len(productions)
    for left, right in (("(", ")"), ("[", "]"), ("{", "}")):
        assert stripped.count(left) == stripped.count(right), (left, right)
    for required in ("create_stmt", "upsert_stmt", "delete_stmt", "schema_admin", "mutation"):
        assert required in productions
    for method in ("append", "remove", "pop", "clear", "push_bounded"):
        assert '"' + method + '"' in grammar



def check_distributed(section):
    ledgers = re.findall(r"```json\n(.*?)\n```", section, re.S)
    assert len(ledgers) == 1
    data = json.loads(ledgers[0])
    assert data["slot_header_offsets"] == list(range(0, 80, 8))
    assert sum(data["slot_header_field_bytes"]) + data["slot_header_padding_bytes"] == 128
    assert data["slot_header_bytes"] == 128 and data["slot_header_alignment"] == 64
    assert data["extent_v3_header_bytes"] == data["extent_v4_header_bytes"] == 128
    assert data["extent_v4_tail_offset"] + sum(data["extent_v4_tail_field_bytes"]) == 128
    assert struct.calcsize("<QQQQQIIIHBBII") == sum(data["core_slot_meta_field_bytes"]) == data["core_slot_meta_bytes"] == 64
    assert sum(data["budget_mib"]) == 16384
    budget, rounding, cores = data["arena_bytes"], data["header_rounding_bytes"], data["reactor_arenas"]
    assert budget == 12 * 1024**3 and rounding == 2 * 1024**2 and cores == 14
    for index, extent in enumerate(data["class_bytes"]):
        assert extent == 4096 << index
        assert budget // extent == data["raw_resident_ceiling"][index]
        assert budget // (extent + 128) == data["header_only_ceiling"][index]
        def used(count):
            q, r = divmod(count, cores)
            aligned = lambda n: ((128 * n + rounding - 1) // rounding) * rounding
            return count * extent + r * aligned(q + 1) + (cores - r) * aligned(q)
        count = data["rounded_resident_ceiling"][index]
        assert used(count) <= budget < used(count + 1)
        assert (count + cores - 1) // cores <= 1 << 20
        row = f"| {index} | {extent:,} | {budget // extent:,} | {budget // (extent + 128):,} | {count:,} |"
        assert row in section, row
    assert sum(data["migration_common_field_bytes"]) == data["migration_common_bytes"] == 120
    assert struct.calcsize("<BBBBHHIIQ16sQQ16sQQ32s") == 120
    assert sum(data["placement_route_field_bytes"]) == data["placement_route_bytes"] == 24
    assert data["wire_v1_request_prefix_bytes"] + data["placement_epoch_bytes"] == data["wire_v2_request_prefix_bytes"] == 96
    assert sum(data["arena_superblock_field_bytes"]) == data["arena_superblock_bytes"] == 4096
    assert data["arena_superblock_copies"] * 4096 == 8192
    assert data["tiny_lfu_rows"] * data["tiny_lfu_row_counters"] * data["tiny_lfu_counter_bits"] // 8 + data["tiny_lfu_doorkeeper_bits"] // 8 == data["tiny_lfu_bytes_per_core"] == 655360
    assert data["quorum"] == data["replica_count"] // 2 + 1 == 2
    assert data["participant_limit"] == 64
    check_offset_table(section, "Wire offset |", "V3 body CRC", 128)
    check_offset_table(section, "V4 offset |", "V4 still persists", 128, initial=80)
    check_offset_table(section, "Byte offset |", "The ABI fingerprint", 4096)
    native_rows = re.findall(r"^\| (\d+) \| (\d+) bits \|", section.split("Do not replace", 1)[0], re.M)
    assert [(int(o), int(w)) for o,w in native_rows] == [(i,64) for i in range(0,80,8)] + [(80,384)]
    extent_code = (ROOT / "ciagomem-wal/src/envelope.rs").read_text()
    assert "IMG_VERSION: u16 = 3" in extent_code and "IMG_WORDS: usize = 16" in extent_code
    lifecycle = (ROOT / "ciagomem-core/src/lifecycle.rs").read_text()
    for current_state in ["Free", "Loading", "Resident", "Replaying", "Flushing", "Quiescing", "Retired"]:
        assert re.search(r"\b" + current_state + r"\b", lifecycle)
    assert "struct CoreSlotMeta" not in (ROOT / "ciagomem-core/src/slot.rs").read_text(), "target metadata silently became implemented"


def main():
    text = (ROOT / "MASTER_SPECIFICATION.md").read_text()
    assert "## 16. R11 self-audit remediation" in text
    section = text.split(HEADING, 1)[1]
    mirror = ROOT.parent / "ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md"
    if mirror.exists():
        assert section == mirror.read_text().split(HEADING, 1)[1], "specification mirrors differ"
    else:
        print("External mirror unavailable in this checkout; mirror comparison skipped.")
    section, distributed = section.split("\n## 15. Concurrency scaling, placement, migration and restart contracts", 1)
    check_distributed(distributed)
    bad_capacity = distributed.replace('"rounded_resident_ceiling":[3045376,', '"rounded_resident_ceiling":[3045377,', 1)
    assert bad_capacity != distributed
    try:
        check_distributed(bad_capacity)
    except AssertionError:
        pass
    else:
        raise AssertionError("capacity checker accepted one slot beyond its arena budget")
    ledger = re.findall(r"```json\n(.*?)\n```", section, re.S)
    assert len(ledger) == 1
    layout = json.loads(ledger[0])
    check_layout(layout)
    slot = (ROOT / "ciagomem-core/src/slot.rs").read_text()
    header = re.search(r"pub struct SlotHeader \{(.*?)\n\}", slot, re.S)
    assert header and len(re.findall(r"pub \w+: (?:AtomicU64|SeqWord)", header[1])) == 10
    assert "size_of::<SlotHeader>() == 128" in slot
    assert '#[repr(C, align(64))]\npub struct SlotHeader' in slot
    # Negative self-check: arithmetic regression must fail, not just print the ledger.
    bad = dict(layout, wire_header_bytes=16)
    try:
        check_layout(bad)
    except AssertionError:
        pass
    else:
        raise AssertionError("layout checker accepted a malformed header")
    check_offset_table(section, "Offset within prefix", "A base image remains", 64)
    check_offset_table(section, "Per-actor batch header is 88 bytes:", "After the header", 88)
    check_offset_table(section, "Byte offset |", "There is no separate", 18)
    check_offset_table(section, "88-byte request prefix", "Exact sum", 88)
    grammar = re.search(r"```ebnf\n(.*?)\n```", section, re.S)
    assert grammar
    check_grammar(grammar[1])
    try:
        check_grammar(grammar[1] + "\nbroken = undefined_rule ;")
    except AssertionError:
        pass
    else:
        raise AssertionError("grammar checker accepted an undefined rule")
    example = re.search(r"<!-- BEGIN CURRENT EMBEDDED EXAMPLE -->\s*```rust\n(.*?)\n```", section, re.S)
    assert example and example[1] == (ROOT / "ciagomem-engine/examples/quickstart.rs").read_text().rstrip()
    for method in ("Assign", "Append", "Remove", "Pop", "Clear", "PushBounded"):
        assert re.search(r"\b" + method + r"\s*\{", section), method
    assert "With 16 classes" not in text
    assert "140 MiB" in text and "**128 bytes**" in text
    print("Checked mirrored sections14–16, native/wire layouts, capacity ceilings, epochs, five classes, EBNF and current example.")
    print("VERDICT: PASS")


if __name__ == "__main__":
    main()
