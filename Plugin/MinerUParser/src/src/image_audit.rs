//! Image inventory is not Figure recognition. Counts deliberately keep separate units.
use crate::assets::rewrite_md_images;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;

const MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const SAMPLE_LIMIT: usize = 20;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImageStats {
    pub schema_version: u32,
    pub disk_image_files: usize,
    /// Syntactic inline ![]() occurrences using the existing asset rewriter.
    /// Not a CommonMark AST: HTML img/reference-style links are not counted.
    pub markdown_inline_image_occurrences: usize,
    pub markdown_unique_local_targets: usize,
    pub markdown_external_occurrences: usize,
    pub content_list_image_blocks: Option<usize>,
    pub content_list_image_paths: Option<usize>,
    pub content_list_captioned_image_blocks: Option<usize>,
    pub table_snapshot_blocks: Option<usize>,
    pub equation_snapshot_blocks: Option<usize>,
    pub missing_local_target_count: usize,
    pub disk_files_without_md_reference: usize,
    /// Unknown until a dedicated caption/figure recognition pass exists.
    pub recognized_figures: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ImageAudit {
    pub image_stats: ImageStats,
    pub markdown_sha256: String,
    pub content_list_sha256: Option<String>,
    pub missing_local_target_sample: Vec<String>,
    pub unreferenced_disk_sample: Vec<String>,
    pub notes: Vec<String>,
}

pub fn summary(stats: Option<&ImageStats>) -> String {
    match stats {
        Some(s) => format!(
            "- **图片口径**：磁盘文件 {} ｜ Markdown内联引用 {} 次（本地目标 {} 个）｜识别image块 {} ｜ 缺失本地目标 {}。这些数量不等价于论文Figure数。\n",
            s.disk_image_files, s.markdown_inline_image_occurrences,
            s.markdown_unique_local_targets,
            s.content_list_image_blocks.map(|n| n.to_string()).unwrap_or_else(|| "未知".into()),
            s.missing_local_target_count,
        ),
        None => "- **图片对账**：尚未生成（旧索引或对账未完成）；可用 reindex 只读预演。图块计数不代表磁盘图片数。\n".into(),
    }
}
fn local_target(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_start_matches('<').trim_end_matches('>').replace('\\', "/");
    if raw.contains(':') || raw.starts_with('/') || raw.starts_with('#') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => { if segments.pop().is_none() { return None; } }
            segment => segments.push(segment),
        }
    }
    Some(segments.join("/"))
}

fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Pure analysis: exact relative paths only. Basename ambiguity is never guessed.
pub fn analyze(md: &str, content: Option<&str>, disk: &BTreeSet<String>) -> ImageAudit {
    let targets = RefCell::new(Vec::new());
    let _ = rewrite_md_images(md, |raw| {
        targets.borrow_mut().push(raw.to_string());
        Some(raw.to_string())
    }, &std::cell::Cell::new(0));
    let targets = targets.into_inner();
    let local: BTreeSet<String> = targets.iter().filter_map(|t| local_target(t)).collect();
    let missing: Vec<String> = local.difference(disk).cloned().collect();
    let unreferenced: Vec<String> = disk.difference(&local).cloned().collect();
    let mut stats = ImageStats {
        schema_version: 1,
        disk_image_files: disk.len(),
        markdown_inline_image_occurrences: targets.len(),
        markdown_unique_local_targets: local.len(),
        markdown_external_occurrences: targets.iter()
            .filter(|t| t.trim().contains(':') || t.trim().starts_with("//")).count(),
        missing_local_target_count: missing.len(),
        disk_files_without_md_reference: unreferenced.len(),
        ..Default::default()
    };
    let mut notes = vec![
        "统计单位不同，数字不相等不等于解析错误；磁盘图片可能含表格、公式及装饰截图。".into(),
        "Markdown统计复用现有内联图片语法扫描器，不含HTML img和引用式图片，也不判定代码示例。".into(),
        "仅精确相对路径匹配；不使用basename猜测，不跟随正文链接读取外部文件。".into(),
    ];
    if let Some(blocks) = content.and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.as_array().cloned())
    {
        let mut image_blocks = 0;
        let mut image_paths = 0;
        let mut captioned = 0;
        let mut tables = 0;
        let mut equations = 0;
        for block in blocks {
            let has_path = block.get("img_path").and_then(Value::as_str)
                .map(|p| !p.trim().is_empty()).unwrap_or(false);
            match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "image" => {
                    image_blocks += 1;
                    image_paths += usize::from(has_path);
                    captioned += usize::from(crate::outline::first_text_list(
                        &block, &["image_caption", "img_caption"]
                    ).is_some());
                }
                "table" if has_path => tables += 1,
                "equation" if has_path => equations += 1,
                _ => {}
            }
        }
        stats.content_list_image_blocks = Some(image_blocks);
        stats.content_list_image_paths = Some(image_paths);
        stats.content_list_captioned_image_blocks = Some(captioned);
        stats.table_snapshot_blocks = Some(tables);
        stats.equation_snapshot_blocks = Some(equations);
    } else {
        notes.push("content_list缺失或无效：相关计数为null，不解释为0。".into());
    }
    ImageAudit {
        image_stats: stats,
        markdown_sha256: digest(md),
        content_list_sha256: content.map(digest),
        missing_local_target_sample: missing.into_iter().take(SAMPLE_LIMIT).collect(),
        unreferenced_disk_sample: unreferenced.into_iter().take(SAMPLE_LIMIT).collect(),
        notes,
    }
}

fn bounded_text(path: &Path) -> Result<String, AppError> {
    let file = fs::File::open(path)
        .map_err(|e| AppError::fix(format!("读取 {} 失败: {}", path.display(), e)))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|e| AppError::fix(format!("读取产物失败: {}", e)))?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(AppError::fix("对账输入超过20MB上限。"));
    }
    String::from_utf8(bytes).map_err(|_| AppError::fix("对账输入不是有效UTF-8。"))
}

/// Fail rather than silently returning a partial inventory on unreadable entries.
/// No symlinks, bounded depth/entry count.
fn walk(root: &Path, dir: &Path, depth: usize, seen: &mut usize, out: &mut BTreeSet<String>)
    -> Result<(), AppError>
{
    if depth > 32 { return Err(AppError::fix("图片目录层级超过32。")); }
    for entry in fs::read_dir(dir).map_err(|e| AppError::fix(format!("扫描图片目录失败: {}", e)))? {
        let entry = entry.map_err(|e| AppError::fix(format!("读取图片条目失败: {}", e)))?;
        *seen += 1;
        if *seen > MAX_ENTRIES { return Err(AppError::fix("图片目录条目超过20000。")); }
        let kind = entry.file_type().map_err(|e| AppError::fix(e.to_string()))?;
        if kind.is_symlink() { return Err(AppError::fix("图片对账不跟随符号链接。")); }
        if kind.is_dir() { walk(root, &entry.path(), depth + 1, seen, out)?; }
        else if kind.is_file() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "svg" | "bmp") {
                out.insert(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn inspect_inputs(file_dir: &Path, image_dir: &Path)
    -> Result<(String, Option<String>, BTreeSet<String>), AppError>
{
    for root in [file_dir, image_dir] {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_symlink() =>
                return Err(AppError::fix("资产根目录不能是符号链接。")),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && root == image_dir => {}
            Err(e) => return Err(AppError::fix(e.to_string())),
        }
    }
    let md_path = file_dir.join("full.md");
    if fs::symlink_metadata(&md_path).map_err(|e| AppError::fix(e.to_string()))?
        .file_type().is_symlink()
    {
        return Err(AppError::fix("full.md不能是符号链接。"));
    }
    let md = bounded_text(&md_path)?;
    let content_path = file_dir.join("content_list.json");
    let content = match fs::symlink_metadata(&content_path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(AppError::fix("content_list不能是符号链接。")),
        Ok(_) => Some(bounded_text(&content_path)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::fix(e.to_string())),
    };
    let mut disk = BTreeSet::new();
    match fs::symlink_metadata(image_dir) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(AppError::fix("图片根目录不能是符号链接。")),
        Ok(_) => walk(image_dir, image_dir, 0, &mut 0, &mut disk)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(AppError::fix(e.to_string())),
    }
    Ok((md, content, disk))
}

pub fn inspect(file_dir: &Path, image_dir: &Path) -> Result<ImageAudit, AppError> {
    let (md, content, disk) = inspect_inputs(file_dir, image_dir)?;
    Ok(analyze(&md, content.as_deref(), &disk))
}

/// Both views consume the same bounded input values, not separate rereads.
fn inspect_inventory(
    file_dir: &Path, image_dir: &Path, legacy: &[crate::protocol::ImageEntry],
    asset_dir: &str,
) -> Result<(ImageAudit, crate::image_inventory::Inventory), AppError> {
    let (md, content, disk) = inspect_inputs(file_dir, image_dir)?;
    let audit = analyze(&md, content.as_deref(), &disk);
    let inventory = crate::image_inventory::merge(
        legacy, &disk, &md, content.as_deref(), asset_dir,
        &crate::assets::UrlContext::from_env(),
    );
    Ok((audit, inventory))
}

/// Update only inventory-owned fields. Preserve unknown root/image extensions.
fn apply_inventory(
    file_dir: &Path, image_dir: &Path, paper_id: &str,
    asset_dir: &str, expected: &str,
) -> Result<Value, AppError> {
    let index_path = file_dir.join("index.json");
    if fs::symlink_metadata(&index_path).map_err(|e| AppError::fix(e.to_string()))?
        .file_type().is_symlink()
    { return Err(AppError::fix("INVALID_INDEX_TARGET: 索引不能是符号链接。")); }
    let raw = bounded_text(&index_path)?;
    let revision = crate::index_store::revision(raw.as_bytes());
    if revision != expected { return Err(AppError::fix("REVISION_CONFLICT: 请重新预演索引。")); }
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|e| AppError::fix(format!("索引JSON无效: {}", e)))?;
    let typed: crate::state::IndexDoc = serde_json::from_str(&raw)
        .map_err(|e| AppError::fix(format!("索引契约无效: {}", e)))?;
    if typed.paper_id != paper_id {
        return Err(AppError::fix("INDEX_ID_MISMATCH: 索引与目标论文不一致。"));
    }
    let (audit, inventory) = inspect_inventory(file_dir, image_dir, &typed.images, asset_dir)?;
    let fingerprint = serde_json::to_vec(&json!({
        "audit": &audit, "inventory": &inventory,
    })).map_err(|e| AppError::fix(e.to_string()))?;
    let additions = inventory.images.iter().skip(typed.images.len())
        .map(serde_json::to_value).collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::fix(e.to_string()))?;
    let object = value.as_object_mut().ok_or_else(|| AppError::fix("索引必须是对象。"))?;
    object.get_mut("images").and_then(Value::as_array_mut)
        .ok_or_else(|| AppError::fix("索引images必须是数组。"))?.extend(additions);
    object.insert("image_stats".into(), json!(&audit.image_stats));
    object.insert("image_provenance".into(), json!(&inventory.provenance));
    // Do not alter original created_at, source_path, state or scientific text.
    object.insert("reindexed_at".into(), json!(crate::state::now_iso()));
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| AppError::fix(e.to_string()))?;
    let receipt = crate::index_store::commit_checked(
        &index_path, &bytes, Some(expected), || {
            let (current_audit, current_inventory) =
                inspect_inventory(file_dir, image_dir, &typed.images, asset_dir)?;
            let current = serde_json::to_vec(&json!({
                "audit": current_audit, "inventory": current_inventory,
            })).map_err(|e| AppError::fix(e.to_string()))?;
            if current != fingerprint {
                return Err(AppError::fix("SOURCE_CHANGED: 正文、结构化产物或图片目录已变化，请重新预演。"));
            }
            Ok(())
        },
    )?;
    Ok(json!({
        "paper_id": paper_id, "applied": true, "revision": receipt.revision,
        "previous_revision": revision, "backup": receipt.backup,
        "unchanged": receipt.unchanged, "appended": inventory.appended,
        "image_total": inventory.images.len(),
    }))
}
/// First rollout is deliberately read-only. Applying an index migration is a
/// separate capability, not a side effect of info/list or this preview.
pub fn cmd_reindex(request: &crate::protocol::Request) -> Result<crate::CommandOutput, AppError> {
    if request.apply && (request.all || request.paper_ids.is_some()
        || request.paper_id.as_deref().map(|s| s == "*" || s.eq_ignore_ascii_case("all")).unwrap_or(true)
        || request.expected_revision.as_deref().map(|s|
            s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit())
        ).unwrap_or(true))
    {
        return Err(AppError::fix("REINDEX_APPLY_REQUIRES_REVISION: 写入仅支持单个paper_id与预演返回的expected_revision，不支持all/paper_ids。"));
    }
    if !request.all && request.paper_id.is_none() && request.paper_ids.is_none() {
        return Err(AppError::fix("reindex需显式指定paper_id/paper_ids或all=true。"));
    }
    let records = crate::select_records(request, crate::library::scan(true))?;
    if records.len() > 200 { return Err(AppError::fix("单次预演最多200条资产，请分批。")); }
    let mut ids = BTreeSet::new();
    for record in &records {
        if !ids.insert(&record.paper_id) {
            return Err(AppError::fix("DUPLICATE_ASSET_DIRS: 同一paper_id存在多个目录，先核对重复目录再预演，避免配错图片根目录。"));
        }
    }
    if request.apply {
        if records.len() != 1 { return Err(AppError::fix("写入必须唯一定位一条资产。")); }
        let record = &records[0];
        let data = apply_inventory(
            &crate::library::resolve_file_dir(&record.paper_id),
            &crate::project_base().join("image/mineru").join(&record.asset_dir),
            &record.paper_id, &record.asset_dir, request.expected_revision.as_deref().unwrap(),
        )?;
        return Ok(crate::CommandOutput::CardData {
            text: format!("索引提交完成：{}。仅更新图片清单与对账字段，备份路径及修订号见data。未调用云端。", record.paper_id),
            data,
        });
    }
    let mut items = Vec::new();
    for record in records {
        let file_dir = crate::library::resolve_file_dir(&record.paper_id);
        let image_dir = crate::project_base().join("image/mineru").join(&record.asset_dir);
        let legacy = record.index.as_ref().map(|i| i.images.as_slice()).unwrap_or(&[]);
        match inspect_inventory(&file_dir, &image_dir, legacy, &record.asset_dir) {
            Ok((audit, inventory)) => items.push(json!({
                "paper_id": record.paper_id, "asset_dir": record.asset_dir,
                "status": "previewed", "legacy_image_count": record.image_count,
                "legacy_indexed_images": record.index.as_ref().map(|i| i.images.len()),
                "index_revision": bounded_text(&file_dir.join("index.json")).ok()
                    .map(|raw| crate::index_store::revision(raw.as_bytes())),
                "audit": audit,
                "inventory_preview": {
                    "candidate_total": inventory.images.len(),
                    "appended": inventory.appended,
                    "legacy_duplicate_paths": inventory.legacy_duplicate_paths,
                    "legacy_positions_preserved": true,
                    "sample": inventory.provenance.iter().skip(legacy.len())
                        .take(SAMPLE_LIMIT).collect::<Vec<_>>(),
                    "sample_limit": SAMPLE_LIMIT,
                },
            })),
            Err(e) => items.push(json!({
                "paper_id": record.paper_id, "asset_dir": record.asset_dir,
                "status": "failed", "error": e.message(),
            })),
        }
    }
    let failed = items.iter().filter(|item| item["status"] == "failed").count();
    Ok(crate::CommandOutput::CardData {
        text: format!("## 重索引预演（只读）\n\n检查 {} 项，失败 {} 项。未修改index/state/正文/图片；未调用云端。详细统计见data.items。", items.len(), failed),
        data: json!({"command":"reindex","dry_run":true,"applied":false,
            "partial":failed > 0,"failed":failed,"items":items}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apply_preserves_extensions_and_backup_and_rejects_stale_revision() {
        let base = std::env::temp_dir().join(format!("mineru_apply_{}", std::process::id()));
        fs::create_dir_all(base.join("images/images")).unwrap();
        let file_dir = base.join("file");
        fs::create_dir_all(&file_dir).unwrap();
        fs::write(file_dir.join("full.md"), "![](images/new.png)").unwrap();
        fs::write(file_dir.join("content_list.json"), "[]").unwrap();
        fs::write(file_dir.join("state.json"), b"untouched-state").unwrap();
        fs::write(base.join("images/images/new.png"), [1,2,3]).unwrap();
        let index = crate::state::IndexDoc {
            paper_id: "paper_test".into(), created_at:"original-time".into(),
            source_path:Some("original-source".into()),
            images:vec![crate::protocol::ImageEntry {
                rel:"images/old.png".into(),url:"old-url".into(),..Default::default()
            }], ..Default::default()
        };
        let mut value = serde_json::to_value(index).unwrap();
        value["custom_extension"] = json!({"keep":true});
        value["images"][0]["custom_image_field"] = json!("preserved");
        let raw = serde_json::to_vec(&value).unwrap();
        fs::write(file_dir.join("index.json"), &raw).unwrap();
        let expected = crate::index_store::revision(&raw);
        let receipt = apply_inventory(&file_dir,&base.join("images"),"paper_test","asset",&expected).unwrap();
        assert_eq!(receipt["appended"],1);
        assert_eq!(fs::read(receipt["backup"].as_str().unwrap()).unwrap(),raw);
        let after: Value = serde_json::from_slice(&fs::read(file_dir.join("index.json")).unwrap()).unwrap();
        assert_eq!(after["custom_extension"],value["custom_extension"]);
        assert_eq!(after["images"][0],value["images"][0]);
        assert_eq!(after["images"][1]["rel"],"images/new.png");
        assert_eq!(after["source_path"],"original-source");
        assert_eq!(after["created_at"],"original-time");
        assert_eq!(fs::read(file_dir.join("state.json")).unwrap(),b"untouched-state");
        assert_eq!(fs::read_to_string(file_dir.join("full.md")).unwrap(),"![](images/new.png)");
        let before_retry = fs::read(file_dir.join("index.json")).unwrap();
        assert!(apply_inventory(&file_dir,&base.join("images"),"paper_test","asset",&expected).is_err());
        assert_eq!(fs::read(file_dir.join("index.json")).unwrap(),before_retry);
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn inspect_is_read_only_and_legacy_index_remains_compatible() {
        let base = std::env::temp_dir().join(format!("mineru_audit_{}", std::process::id()));
        fs::create_dir_all(base.join("images/images")).unwrap();
        let file_dir = base.join("file");
        fs::create_dir_all(&file_dir).unwrap();
        fs::write(file_dir.join("full.md"), "![](images/a.png)").unwrap();
        fs::write(file_dir.join("content_list.json"), "[]").unwrap();
        let legacy = crate::state::IndexDoc { paper_id: "paper_test".into(), ..Default::default() };
        let raw = serde_json::to_string(&legacy).unwrap();
        assert!(!raw.contains("image_stats"));
        fs::write(file_dir.join("index.json"), &raw).unwrap();
        fs::write(file_dir.join("state.json"), r#"{"state":"indexed"}"#).unwrap();
        fs::write(base.join("images/images/a.png"), [1, 2, 3]).unwrap();
        let before: Vec<_> = ["index.json","state.json","full.md","content_list.json"]
            .iter().map(|name| fs::read(file_dir.join(name)).unwrap()).collect();
        let audit = inspect(&file_dir, &base.join("images")).unwrap();
        assert_eq!(audit.image_stats.disk_image_files, 1);
        assert_eq!(audit.image_stats.missing_local_target_count, 0);
        for (name, bytes) in ["index.json","state.json","full.md","content_list.json"].iter().zip(before) {
            assert_eq!(fs::read(file_dir.join(name)).unwrap(), bytes);
        }
        let loaded: crate::state::IndexDoc = serde_json::from_str(&raw).unwrap();
        assert!(loaded.image_stats.is_none());
        let req = serde_json::from_value(json!({"command":"reindex","all":true,"apply":true})).unwrap();
        assert!(cmd_reindex(&req).is_err());
        let req = serde_json::from_value(json!({"command":"reindex"})).unwrap();
        assert!(cmd_reindex(&req).is_err());
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn distinct_units_and_unknown_content() {
        let disk = ["images/a.png".into(), "images/table.png".into()].into_iter().collect();
        let md = "![](images/a.png) ![](images/a.png) ![](images/missing.png) ![](https://x/a.png)";
        let content = r#"[{"type":"image","img_path":"images/a.png","image_caption":["Fig 1"]},{"type":"table","img_path":"images/table.png"},{"type":"equation"}]"#;
        let audit = analyze(md, Some(content), &disk);
        assert_eq!(audit.image_stats.disk_image_files, 2);
        assert_eq!(audit.image_stats.markdown_inline_image_occurrences, 4);
        assert_eq!(audit.image_stats.markdown_unique_local_targets, 2);
        assert_eq!(audit.image_stats.markdown_external_occurrences, 1);
        assert_eq!(audit.image_stats.content_list_image_blocks, Some(1));
        assert_eq!(audit.image_stats.table_snapshot_blocks, Some(1));
        assert_eq!(audit.image_stats.equation_snapshot_blocks, Some(0));
        assert_eq!(audit.image_stats.missing_local_target_count, 1);
        assert_eq!(audit.image_stats.disk_files_without_md_reference, 1);
        assert_eq!(audit.image_stats.recognized_figures, None);
        assert_eq!(analyze(md, Some("broken"), &disk).image_stats.content_list_image_blocks, None);
        assert_eq!(analyze(md, Some("[]"), &disk).image_stats.content_list_image_blocks, Some(0));
        assert_eq!(local_target("../../outside.png"), None);
    }
}