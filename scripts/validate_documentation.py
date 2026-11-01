#!/usr/bin/env python3
"""Validate current/target documentation boundaries and target protocol arithmetic."""
from pathlib import Path
import re
import struct
from validate_spec_contract import check_grammar

ROOT = Path(__file__).resolve().parents[1]
GUIDES = ['README.md', 'INSTALL.md', 'ABOUT.md', 'EXAMPLES.md', 'USECASE.md',
          'CIAQL_REFERENCE.md', 'SECURITY.md', 'CONFIGURATION.md', 'DEBUGGING.md', 'TESTING.md']
HEADING = '## 17. Complete application and operator documentation contract'


def validate_contract(spec):
    section = spec.split(HEADING, 1)[1]
    grammar = re.search(r'```ebnf\n(.*?)\n```', section, re.S)
    assert grammar, 'missing consolidated grammar'
    check_grammar(grammar[1])
    for rule in ['mutation_command', 'join_stage', 'link_stmt', 'search_stage',
                 'similarity_stage', 'metadata_path', 'result_path', 'tx_source']:
        assert re.search(r'^' + rule + r'\s*=', grammar[1], re.M), rule
    for token in ['"count_distinct"', '"p95"', '"contains"', '"in"']:
        assert token in grammar[1], token
    assert '"|->"' not in grammar[1]
    assert struct.calcsize('<BQBBHQ16s') == 37  # LINK operation body.
    assert struct.calcsize('<BHIIB') == 12    # Diagnostic prefix, before related/message.
    assert struct.calcsize('<BBQH32s') == 44  # Capability fixed framing incl signature.
    assert 10432 + 2048 + 2048 + 1036 + 820 == 16384
    assert 12288 + 1536 + 1024 + 768 + 256 + 512 == 16384
    assert 64 * 1024**2 // 4096 == 16384
    assert 1_000_000 * 8 / 1024**2 > 7.629
    for required in ['conditional_mutate_v1', 'ConditionFailed', 'RetryTooOld', 'diagnostic_v1',
                     'CIAGOMEM_MODE', 'CIAGOMEM_PROFILE', 'CIAGOMEM_KEYRING_FILE',
                     '17.8 General-purpose workload blueprints', 'not shipped', 'not a restriction']:
        assert required.lower() in section.lower(), required
    return section


def main():
    spec = (ROOT / 'MASTER_SPECIFICATION.md').read_text()
    validate_contract(spec)
    # An undefined grammar production must not pass the new contract gate.
    bad = spec.replace('join_stage      =', 'join_stage      = missing_rule,', 1)
    try:
        validate_contract(bad)
    except AssertionError:
        pass
    else:
        raise AssertionError('accepted undefined grammar rule')
    mirror = ROOT.parent / 'ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md'
    if mirror.exists():
        assert mirror.read_bytes() == (ROOT / 'MASTER_SPECIFICATION.md').read_bytes(), 'full spec mirrors differ'
    for name in GUIDES:
        path = ROOT / name
        text = path.read_text()
        assert text.count('```') % 2 == 0, ('unclosed fence', name)
        assert re.search(r'\btarget\b', text, re.I), ('missing target status', name)
        for target in re.findall(r'\]\((<[^>]+>|[^)]+)\)', text):
            target = target.strip('<>')
            if target.startswith(('https:', 'http:', '#')):
                continue
            assert (path.parent / target.split('#', 1)[0]).exists(), (name, target)
        # Markdown hyperlinks pasted into literal code are not compilable imports/commands.
        for language, body in re.findall(r'```(\w+)\n(.*?)\n```', text, re.S):
            if language in {'sh', 'bash', 'go', 'rust', 'typescript', 'toml'}:
                assert not re.search(r'\[[^\]]+\]\(https?://', body), ('hyperlink in code', name)
    for name in ['USECASE.md', 'ABOUT.md', 'EXAMPLES.md']:
        assert '**Implementation status:**' in (ROOT / name).read_text()
    assert 'RetryTooOld10' in (ROOT / 'DEBUGGING.md').read_text()
    assert 'never automatically repeat' in (ROOT / 'DEBUGGING.md').read_text()
    orchestration = spec.split('## 18. Sub-agent memory orchestration and hybrid context assembly', 1)[1]
    for required in ['MemoryBroker', 'Label::derived', 'External', 'MemoryJournal', 'source', 'hybrid_cost', 'evidence_label']:
        assert required in orchestration, required
    usecase = (ROOT / 'USECASE.md').read_text()
    assert '## 5. Sub-Agent Memory Orchestration: The Zero-Token Waste Pattern' in usecase
    assert 'model tokens still apply' in usecase
    assert (ROOT / 'ciagomem-engine/examples/memory_orchestration.rs').exists()
    workflow = (ROOT / '.github/workflows/rust.yml').read_text()
    assert 'python3 scripts/validate_documentation.py' in workflow
    service = spec.split('## 20. Implemented single-node TLS reactor and durable client retries', 1)[1]
    assert struct.calcsize('<HBBHIII') == 18
    assert struct.calcsize('<BBH8sIQQ16s16s16sII') == 88
    assert struct.calcsize('<HBBIQ16s') == 32
    for required in ['CMEB0002', 'CMEB0001', 'CIDFILE1', 'RetryTooOld', 'ClientId',
                     '3.5.9', 'not a per-actor increment', 'High-water marks do not automatically expire',
                     'same TLS/authentication/framing/reactor implementation']:
        assert required in service, ('service contract missing', required)
    wire = (ROOT / 'ciagomem-engine/src/wire.rs').read_text()
    for constant, value in [('HEADER',18),('PREFIX',88),('RESPONSE',32)]:
        assert f'pub const {constant}: usize = {value};' in wire
    assert 'pub const VERSION: u8 = 3;' in wire
    assert (ROOT / 'ciagomem-engine/src/bin/ciagomem-server.rs').exists()
    assert (ROOT / 'ciagomem-engine/examples/network_client.rs').exists()
    assert 'Install pinned TLS provider' in workflow
    print('Checked full specification mirror, consolidated grammar, wire sizes, budgets, guide status/links and CI gate.')
    print('VERDICT: PASS')


if __name__ == '__main__':
    main()
