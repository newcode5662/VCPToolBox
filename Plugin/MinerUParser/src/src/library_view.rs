//! Machine library metadata, separate from clipped agent cards and PDF IO.
use crate::{error::AppError, library::{self, PaperRecord}};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn preferred(a: &PaperRecord, b: &PaperRecord) -> bool {
    if a.has_index != b.has_index { return a.has_index; }
    if a.legacy_dir != b.legacy_dir { return !a.legacy_dir; }
    if a.updated_at != b.updated_at { return a.updated_at > b.updated_at; }
    a.asset_dir < b.asset_dir
}

fn build(records: Vec<PaperRecord>) -> Result<Value, AppError> {
    let scanned = records.len();
    let mut groups: BTreeMap<String, Vec<PaperRecord>> = BTreeMap::new();
    for record in records { groups.entry(record.paper_id.clone()).or_default().push(record); }
    let mut papers = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a,b| if preferred(a,b) { std::cmp::Ordering::Less }
            else if preferred(b,a) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
        let record = &group[0];
        let mut value = crate::record_json(record, false);
        value["file_name"] = json!(record.file_name);
        value["created_at"] = json!(record.created_at);
        value["has_index"] = json!(record.has_index);
        value["has_state"] = json!(record.has_state);
        value["duplicate_dirs"] = json!(group.iter().skip(1).map(|r| &r.asset_dir).collect::<Vec<_>>());
        value["outline_count"] = json!(record.index.as_ref().map(|i| i.outline.len()));
        value["image_stats"] = json!(record.index.as_ref().and_then(|i| i.image_stats.as_ref()));
        value["warnings"] = json!(record.index.as_ref().map(|i| &i.warnings));
        value["errors"] = json!(record.index.as_ref().map(|i| &i.errors));
        papers.push(value);
    }
    // Stable by ID: UI chooses visual sorting; digest must not depend on readdir order.
    let encoded = serde_json::to_vec(&papers).map_err(|e| AppError::fix(e.to_string()))?;
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(AppError::fix("LIBRARY_VIEW_TOO_LARGE: 元数据超过4MB；未返回截断库。"));
    }
    let revision = hex::encode(Sha256::digest(&encoded));
    Ok(json!({
        "schema":"mineru.library-view","schema_version":1,
        "complete":true,"truncated":false,"total":papers.len(),
        "asset_directory_count":scanned,"revision":revision,
        "consistency":"best_effort_scan",
        "papers":papers,
        "note":"只读元数据扫描，非跨文件事务快照；不含正文/完整大纲/图片清单或本机PDF绑定。",
    }))
}

/// Full metadata for one canonical asset; no scientific text/PDF bytes.
pub fn paper_command(request: &crate::protocol::Request) -> Result<crate::CommandOutput, AppError> {
    let id = request.paper_id.as_deref()
        .ok_or_else(|| AppError::fix("paper_view需要精确paper_id。"))?;
    let base = crate::project_base();
    let view = build(library::scan_checked(&base)?)?;
    let paper = view["papers"].as_array().unwrap().iter()
        .find(|paper| paper["paper_id"].as_str() == Some(id))
        .ok_or_else(|| AppError::fix("PAPER_NOT_FOUND: 未找到论文。"))?;
    let dir = base.join("file/mineru").join(paper["asset_dir"].as_str().unwrap());
    fn read_json(path: &std::path::Path) -> Result<Value, AppError> {
        use std::io::Read;
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() =>
                return Err(AppError::fix("PAPER_INVALID_METADATA: 必须为普通文件。")),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
            Err(e) => return Err(AppError::fix(e.to_string())),
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path).map_err(|e| AppError::fix(e.to_string()))?
            .take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)
            .map_err(|e| AppError::fix(e.to_string()))?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(AppError::fix("PAPER_VIEW_TOO_LARGE: 单项元数据超过4MB。"));
        }
        serde_json::from_slice(&bytes).map_err(|e| AppError::fix(e.to_string()))
    }
    let index = read_json(&dir.join("index.json"))?;
    let state = read_json(&dir.join("state.json"))?;
    for value in [&index, &state] {
        if !value.is_null() && value["paper_id"].as_str() != Some(id) {
            return Err(AppError::fix("PAPER_CHANGED: 元数据身份发生变化。"));
        }
    }
    if paper["has_index"] == true && index.is_null()
        || paper["has_state"] == true && state.is_null()
    { return Err(AppError::fix("PAPER_CHANGED: 扫描期间产物消失。")); }
    let mut data = json!({
        "schema":"mineru.paper-view","schema_version":1,
        "complete":true,"truncated":false,"paper":paper,"index":index,"state":state,
        "consistency":"best_effort_scan",
    });
    let encoded = serde_json::to_vec(&data).map_err(|e| AppError::fix(e.to_string()))?;
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(AppError::fix("PAPER_VIEW_TOO_LARGE: 总元数据超过4MB，未返回截断详情。"));
    }
    data["revision"] = json!(hex::encode(Sha256::digest(&encoded)));
    Ok(crate::CommandOutput::CardData {
        text: format!("完整详情元数据：{}；不含正文或PDF。", id), data,
    })
}
pub fn command() -> Result<crate::CommandOutput, AppError> {
    let data = build(library::scan_checked(&crate::project_base())?)?;
    Ok(crate::CommandOutput::CardData {
        text: format!("机器库视图 v1：{} 篇，完整元数据；正文与完整详情需另取。", data["total"]),
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_dedup_and_empty_complete_view() {
        let empty = build(vec![]).unwrap();
        assert_eq!(empty["total"],0);
        assert_eq!(empty["complete"],true);
        let a = PaperRecord { paper_id:"paper_a".into(),asset_dir:"paper_a".into(),
            legacy_dir:true,has_index:true,group:"indexed".into(), ..Default::default() };
        let b = PaperRecord { asset_dir:"Title__paper_a".into(),legacy_dir:false,..a.clone() };
        let first = build(vec![a.clone(),b.clone()]).unwrap();
        let second = build(vec![b,a]).unwrap();
        assert_eq!(first["revision"],second["revision"]);
        assert_eq!(first["total"],1);
        assert_eq!(first["asset_directory_count"],2);
        assert_eq!(first["papers"][0]["asset_dir"],"Title__paper_a");
        assert_eq!(first["papers"][0]["duplicate_dirs"][0],"paper_a");
        assert!(first["papers"][0].get("images").is_none());
    }
    #[test]
    fn strict_scan_rejects_corrupt_index_instead_of_reporting_indexed() {
        let base = std::env::temp_dir().join(format!("mineru_view_{}",std::process::id()));
        let dir = base.join("file/mineru/paper_a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"),"{broken").unwrap();
        assert!(library::scan_checked(&base).unwrap_err().message().contains("INVALID_JSON"));
        std::fs::remove_file(dir.join("index.json")).unwrap();
        assert!(library::scan_checked(&base).unwrap().is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }
}