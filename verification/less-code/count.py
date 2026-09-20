#!/usr/bin/env python3
"""External measurement tool: tree-sitter==0.26.0, tree-sitter-rust==0.24.2.

Run against working tree: python count.py
Run against a revision: python count.py REV
No application/build dependencies are added.
"""
import re
import subprocess
import sys
from pathlib import Path
from tree_sitter import Language, Parser
import tree_sitter_rust

PARSER = Parser(Language(tree_sitter_rust.language()))


def count(source):
    tree = PARSER.parse(source)
    if tree.root_node.has_error:
        raise ValueError('Rust parse error; refusing an approximate count')
    masked = bytearray(source)

    def erase(node):
        for i in range(node.start_byte, node.end_byte):
            if masked[i] not in (10, 13):
                masked[i] = 32

    def walk(node):
        pending = []
        test = False
        for child in node.children:
            if child.type in ('line_comment', 'block_comment'):
                erase(child)
            elif child.type == 'attribute_item':
                pending.append(child)
                test |= bool(re.fullmatch(rb'#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]', child.text))
            else:
                if test:
                    for attr in pending:
                        erase(attr)
                    erase(child)
                else:
                    walk(child)
                pending = []
                test = False
    walk(tree.root_node)
    return sum(bool(line.strip()) for line in masked.splitlines())


def production(path):
    p = Path(path)
    return p.suffix == '.rs' and p.name not in ('tests.rs', 'testing.rs') and 'tests' not in p.parts


def self_test():
    assert count(b'#[cfg(test)]\nuse a::b;\nfn main() {}\n') == 1
    assert count(b'#[cfg(test)]\nmod tests { fn test() {} }\nfn main() {}\n') == 1
    assert count(b'/* outer /* inner */ comment */\nfn main() {} // tail\n') == 1
    assert count(b'const S: &str = r#"/* not a comment */\n} // still string\n"#;\n') == 3
    assert count(b'#[derive(Clone)]\nstruct A;\n') == 2
    assert production('src/nested/module.rs')
    assert not production('src/nested/tests.rs')
    assert not production('src/nested/tests/module.rs')
    assert not production('src/testing.rs')


if __name__ == '__main__':
    self_test()
    revision = sys.argv[1] if len(sys.argv) > 1 else None
    if revision:
        paths = subprocess.check_output(['git', 'ls-tree', '-r', '--name-only', revision, '--', 'src/']).decode().splitlines()
        sources = ((p, subprocess.check_output(['git', 'show', f'{revision}:{p}'])) for p in paths if production(p))
    else:
        sources = ((str(p), p.read_bytes()) for p in sorted(Path('src').rglob('*.rs')) if production(p))
    print(sum(count(source) for _, source in sources))
