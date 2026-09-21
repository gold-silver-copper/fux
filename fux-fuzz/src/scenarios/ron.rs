//! Entity blocks of a saved scene file, for scenarios that edit one.
use super::*;

/// One `    <id>: (` entity block of a saved scene, with its id.
pub(super) struct Block {
    pub id: u64,
    pub text: String,
}
pub(super) fn blocks(ron: &str) -> Result<(String, Vec<Block>, String)> {
    let start = ron.find("  entities: {\n").ok_or("no entities")? + "  entities: {\n".len();
    let end = ron.rfind("\n  },\n").ok_or("no entities end")?;
    let head = ron.get(..start).ok_or("head")?.to_owned();
    let tail = ron.get(end..).ok_or("tail")?.to_owned();
    let body = ron.get(start..end).ok_or("body")?;
    let mut out = Vec::new();
    for piece in body.split("\n    ),").filter(|p| !p.trim().is_empty()) {
        let piece = piece.trim_start_matches('\n');
        let (id_text, rest) = piece.split_once(": (\n").ok_or("block id")?;
        out.push(Block {
            id: id_text.trim().parse()?,
            text: rest.to_owned(),
        });
    }
    Ok((head, out, tail))
}
pub(super) fn assemble(head: &str, blocks: &[Block], tail: &str) -> String {
    let mut out = head.to_owned();
    for b in blocks {
        out.push_str(&format!("    {}: (\n{}\n    ),\n", b.id, b.text));
    }
    out.push_str(tail.trim_start_matches('\n'));
    out
}
pub(super) fn has(block: &Block, component: &str) -> bool {
    block.text.contains(&format!("\"{component}\""))
}
