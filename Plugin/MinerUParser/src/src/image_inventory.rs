//! Stable image inventory: preserve legacy positions, append exact disk paths.
use crate::assets::{rewrite_md_images, UrlContext};
use crate::protocol::ImageEntry;
use serde::Serialize;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
pub struct BlockLink {
    pub block_index: usize,
    pub kind: String,
    pub page_idx: Option<u64>,
    /// Raw producer coordinates. No assumed unit or normalization.
    pub bbox: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ImageProvenance {
    pub rel: String,
    pub on_disk: bool,
    pub sources: Vec<String>,
    pub blocks: Vec<BlockLink>,
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub images: Vec<ImageEntry>,
    pub provenance: Vec<ImageProvenance>,
    pub appended: usize,
    pub legacy_duplicate_paths: usize,
}

/// Strict local asset path. Never collapse .. or interpret URLs as file paths.
pub fn exact_relative(raw: &str) -> Option<String> {
    let value = raw.trim().replace('\\', "/");
    if value.is_empty() || value.starts_with('/') || value.contains(':')
        || value.contains('\0') || value.split('/').any(|p| p == "..")
    { return None; }
    let value = value.split('/').filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>().join("/");
    if value.is_empty() { None } else { Some(value) }
}

/// Only disk-backed candidates are appended. Missing legacy entries stay in
/// place so callers' historical numeric selectors never shift.
pub fn merge(
    legacy: &[ImageEntry], disk: &BTreeSet<String>, md: &str,
    content: Option<&str>, asset_dir: &str, urls: &UrlContext,
) -> Inventory {
    let md_paths = RefCell::new(BTreeSet::new());
    let _ = rewrite_md_images(md, |target| {
        if let Some(rel) = exact_relative(target) { md_paths.borrow_mut().insert(rel); }
        Some(target.to_string())
    }, &std::cell::Cell::new(0));
    let md_paths = md_paths.into_inner();
    let mut links: BTreeMap<String, Vec<BlockLink>> = BTreeMap::new();
    let mut captions: BTreeMap<String, String> = BTreeMap::new();
    if let Some(Value::Array(blocks)) = content.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        for (index, block) in blocks.iter().enumerate() {
            let Some(rel) = block.get("img_path").and_then(Value::as_str).and_then(exact_relative) else { continue; };
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("unknown");
            let keys: &[&str] = match kind {
                "image" => &["image_caption", "img_caption"],
                "table" => &["table_caption"],
                "equation" => &["equation_caption"],
                _ => &[],
            };
            if let Some(caption) = crate::outline::first_text_list(block, keys) {
                captions.entry(rel.clone()).or_insert(caption);
            }
            let bbox = block.get("bbox").filter(|value| value.as_array().map(|a|
                a.len() == 4 && a.iter().all(|n| n.as_f64().map(f64::is_finite).unwrap_or(false))
            ).unwrap_or(false)).cloned();
            links.entry(rel).or_default().push(BlockLink {
                block_index: index, kind: kind.into(),
                page_idx: block.get("page_idx").and_then(Value::as_u64), bbox,
            });
        }
    }
    let mut images = legacy.to_vec();
    let mut seen = BTreeSet::new();
    let mut duplicates = 0;
    for image in legacy {
        if let Some(rel) = exact_relative(&image.rel) {
            if !seen.insert(rel) { duplicates += 1; }
        }
    }
    let mut appended = 0;
    for rel in disk {
        if exact_relative(rel).as_deref() != Some(rel.as_str()) || !seen.insert(rel.clone()) { continue; }
        let blocks = links.get(rel);
        // A shared screenshot may represent multiple pages. Do not choose one.
        let pages: BTreeSet<u64> = blocks.into_iter().flatten().filter_map(|b| b.page_idx).collect();
        images.push(ImageEntry {
            rel: rel.clone(), url: urls.image_url(asset_dir, rel),
            caption: captions.get(rel).cloned(),
            page_idx: if pages.len() == 1 { pages.first().copied() } else { None },
        });
        appended += 1;
    }
    let provenance = images.iter().map(|image| {
        let rel = exact_relative(&image.rel).unwrap_or_else(|| image.rel.clone());
        let mut sources = Vec::new();
        if legacy.iter().any(|old| exact_relative(&old.rel).as_deref() == Some(rel.as_str())) {
            sources.push("legacy_index".into());
        }
        if disk.contains(&rel) { sources.push("disk".into()); }
        if md_paths.contains(&rel) { sources.push("markdown_inline".into()); }
        if links.contains_key(&rel) { sources.push("content_list".into()); }
        ImageProvenance {
            on_disk: disk.contains(&rel), sources,
            blocks: links.get(&rel).map(|blocks| blocks.iter().map(|b| BlockLink {
                block_index: b.block_index, kind: b.kind.clone(),
                page_idx: b.page_idx, bbox: b.bbox.clone(),
            }).collect()).unwrap_or_default(),
            rel,
        }
    }).collect();
    Inventory { images, provenance, appended, legacy_duplicate_paths: duplicates }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn urls() -> UrlContext {
        UrlContext { port: "6005".into(), image_key: "test".into(), file_key: "test".into() }
    }
    #[test]
    fn legacy_order_missing_entries_and_repeat_merge_are_stable() {
        let old = vec![
            ImageEntry { rel:"images/z.png".into(), url:"old-z".into(), caption:Some("Keep".into()), page_idx:Some(8) },
            ImageEntry { rel:"images/missing.png".into(), url:"old-missing".into(), ..Default::default() },
        ];
        let disk = ["images/z.png".into(),"images/a.png".into()].into_iter().collect();
        let content = r#"[{"type":"table","img_path":"images/a.png","table_caption":["Table 1"],"page_idx":2,"bbox":[1,2,3,4]}]"#;
        let first = merge(&old, &disk, "![](images/a.png)", Some(content), "asset", &urls());
        assert_eq!(first.appended,1);
        assert_eq!(first.images[0].url,"old-z");
        assert_eq!(first.images[0].page_idx,Some(8));
        assert_eq!(first.images[1].rel,"images/missing.png");
        assert!(!first.provenance[1].on_disk);
        assert_eq!(first.images[2].caption.as_deref(),Some("Table 1"));
        assert_eq!(first.provenance[2].blocks[0].kind,"table");
        assert_eq!(first.provenance[2].blocks[0].bbox,Some(serde_json::json!([1,2,3,4])));
        let second = merge(&first.images,&disk,"",Some(content),"asset",&urls());
        assert_eq!(second.appended,0);
        assert_eq!(serde_json::to_value(first.images).unwrap(),serde_json::to_value(second.images).unwrap());
    }
    #[test]
    fn no_basename_guessing_no_fabricated_pages_or_captions() {
        let disk = ["a/same.png".into(),"b/same.png".into(),"orphan.png".into()].into_iter().collect();
        let content = r#"[{"type":"image","img_path":"same.png","page_idx":5},{"type":"table","img_path":"a/same.png","page_idx":1},{"type":"table","img_path":"a/same.png","page_idx":2}]"#;
        let result = merge(&[],&disk,"![](../escape.png)",Some(content),"asset",&urls());
        assert_eq!(result.images.len(),3);
        assert!(result.images.iter().all(|i| i.page_idx.is_none() && i.caption.is_none()));
        assert!(result.provenance[1].blocks.is_empty());
        assert_eq!(exact_relative("../escape"),None);
        assert_eq!(exact_relative("https://x/y"),None);
        assert_eq!(exact_relative("C:\\x"),None);
    }
}