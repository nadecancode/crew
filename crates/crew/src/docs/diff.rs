//! A bounded in-process line diff. Large edit matrices use a valid coarse
//! replacement with `truncated`, never an external diff/textconv process.
use crew_core::rpc::docs::{DocDiff, DocFailureCode, DocVersion};
use crew_core::rpc::features::Text;

pub fn compare(
    left: DocVersion,
    a: &str,
    right: DocVersion,
    b: &str,
) -> Result<DocDiff, DocFailureCode> {
    let a: Vec<_> = a.split_inclusive('\n').collect();
    let b: Vec<_> = b.split_inclusive('\n').collect();
    if a.len() > 8192 || b.len() > 8192 {
        return Err(DocFailureCode::TooLarge);
    }
    let mut prefix = 0;
    while prefix < a.len().min(b.len()) && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len().min(b.len()) - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let x = &a[prefix..a.len() - suffix];
    let y = &b[prefix..b.len() - suffix];
    let coarse = (x.len() + 1).saturating_mul(y.len() + 1) > 1_000_000;
    let mut ops: Vec<(char, &str)> = Vec::new();
    if x.is_empty() && y.is_empty() {
        return Ok(DocDiff {
            left,
            right,
            unified_diff: Text::default(),
            added_lines: 0,
            removed_lines: 0,
            truncated: false,
        });
    }
    let start = prefix.saturating_sub(3);
    for line in &a[start..prefix] {
        ops.push((' ', line));
    }
    if coarse {
        ops.extend(x.iter().map(|s| ('-', *s)));
        ops.extend(y.iter().map(|s| ('+', *s)));
    } else {
        let columns = y.len() + 1;
        let mut lcs = vec![0u16; (x.len() + 1) * columns];
        for i in (0..x.len()).rev() {
            for j in (0..y.len()).rev() {
                lcs[i * columns + j] = if x[i] == y[j] {
                    1 + lcs[(i + 1) * columns + j + 1]
                } else {
                    lcs[(i + 1) * columns + j].max(lcs[i * columns + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < x.len() || j < y.len() {
            if i < x.len() && j < y.len() && x[i] == y[j] {
                ops.push((' ', x[i]));
                i += 1;
                j += 1;
            } else if j < y.len()
                && (i == x.len() || lcs[i * columns + j + 1] >= lcs[(i + 1) * columns + j])
            {
                ops.push(('+', y[j]));
                j += 1;
            } else {
                ops.push(('-', x[i]));
                i += 1;
            }
        }
    }
    let end = suffix.min(3);
    for line in &a[a.len() - suffix..a.len() - suffix + end] {
        ops.push((' ', line));
    }
    let removed_lines = ops.iter().filter(|(op, _)| *op == '-').count() as u32;
    let added_lines = ops.iter().filter(|(op, _)| *op == '+').count() as u32;
    let old = ops.iter().filter(|(op, _)| *op != '+').count();
    let new = ops.iter().filter(|(op, _)| *op != '-').count();
    let mut out = format!(
        "--- {}\n+++ {}\n@@ -{},{} +{},{} @@\n",
        left.id.as_str(),
        right.id.as_str(),
        if old == 0 { start } else { start + 1 },
        old,
        if new == 0 { start } else { start + 1 },
        new
    );
    let mut truncated = coarse;
    for (op, line) in ops {
        let extra = if line.ends_with('\n') { 0 } else { 29 };
        if out.len() + line.len() + 1 + extra > 65536 {
            truncated = true;
            break;
        }
        out.push(op);
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
    Ok(DocDiff {
        left,
        right,
        unified_diff: Text::new(out).map_err(|_| DocFailureCode::TooLarge)?,
        added_lines,
        removed_lines,
        truncated,
    })
}
