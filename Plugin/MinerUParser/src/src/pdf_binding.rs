//! Local reading attachment, never a replacement for parse provenance.
use crate::error::AppError;
use crate::protocol::Request;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;

const MAX_PDF_BYTES: u64 = 200 * 1024 * 1024;
const MAX_BINDING_BYTES: u64 = 64 * 1024;

fn binding_bytes(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() =>
            return Err(AppError::fix("INVALID_BINDING_FILE: 必须是普通文件。")),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::fix(e.to_string())),
    }
    let mut bytes = Vec::new();
    fs::File::open(path).map_err(|e| AppError::fix(e.to_string()))?
        .take(MAX_BINDING_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|e| AppError::fix(e.to_string()))?;
    if bytes.len() as u64 > MAX_BINDING_BYTES {
        return Err(AppError::fix("绑定文件超过64KB。"));
    }
    Ok(Some(bytes))
}

fn binding_revision(bytes: Option<&[u8]>) -> String {
    bytes.map(crate::index_store::revision).unwrap_or_else(|| "missing".into())
}

/// Header + bounded full digest, not an assertion of PDF layout validity.
fn inspect_pdf(path: &Path) -> Result<(String, String, u64), AppError> {
    if !path.is_absolute() || path.extension().and_then(|s| s.to_str())
        .map(|s| !s.eq_ignore_ascii_case("pdf")).unwrap_or(true)
    { return Err(AppError::fix("PDF_PATH_REQUIRED: 需提供绝对PDF路径。")); }
    let canonical = fs::canonicalize(path).map_err(|e| AppError::fix(e.to_string()))?;
    let mut file = fs::File::open(&canonical).map_err(|e| AppError::fix(e.to_string()))?;
    let meta = file.metadata().map_err(|e| AppError::fix(e.to_string()))?;
    if !meta.is_file() || meta.len() > MAX_PDF_BYTES {
        return Err(AppError::fix("PDF_SIZE_LIMIT: PDF必须是200MB以内普通文件。"));
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut header = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| AppError::fix(e.to_string()))?;
        if n == 0 { break; }
        total += n as u64;
        if total > MAX_PDF_BYTES { return Err(AppError::fix("PDF_SIZE_LIMIT: 读取时超过200MB。")); }
        if header.len() < 5 {
            header.extend_from_slice(&buffer[..n.min(5 - header.len())]);
        }
        hash.update(&buffer[..n]);
    }
    if header != b"%PDF-" { return Err(AppError::fix("INVALID_PDF_HEADER: 未找到PDF文件头。")); }
    if total != meta.len() { return Err(AppError::fix("PDF_CHANGED: 读取时文件大小改变。")); }
    Ok((canonical.to_string_lossy().into_owned(), hex::encode(hash.finalize()), total))
}

fn plan(file_dir: &Path, paper_id: &str, pdf: &Path) -> Result<Value, AppError> {
    let (path, sha256, bytes) = inspect_pdf(pdf)?;
    let existing = binding_bytes(&file_dir.join("pdf-binding.json"))?;
    let id_matches = paper_id == format!("paper_{}", &sha256[..16]);
    Ok(json!({
        "schema_version":1, "paper_id":paper_id, "path":path,
        "sha256":sha256, "bytes":bytes,
        "verification":if id_matches {"content_id_prefix_match"} else {"unverified_identity"},
        "id_prefix_matches":id_matches,
        "can_apply":id_matches,
        "binding_revision":binding_revision(existing.as_deref()),
        "note":"仅核验PDF文件头与内容哈希，不代表版面解析有效；绑定只用于本机阅读。",
    }))
}

fn apply(
    dir: &Path, paper_id: &str, pdf: &Path, expected: &str, expected_pdf: &str,
) -> Result<Value, AppError> {
    let candidate = plan(dir, paper_id, pdf)?;
    if candidate["can_apply"] != true {
        return Err(AppError::fix("PDF_IDENTITY_UNVERIFIED: 内容ID不匹配；URL或不同版本需单独人工确认流程，当前不自动绑定。"));
    }
    if candidate["sha256"].as_str() != Some(expected_pdf) {
        return Err(AppError::fix("PDF_CHANGED: 与预演PDF哈希不同，请重新核验。"));
    }
    let path = dir.join("pdf-binding.json");
    let old = binding_bytes(&path)?;
    if binding_revision(old.as_deref()) != expected {
        return Err(AppError::fix("BINDING_REVISION_CONFLICT: 阅读绑定已改变，请重新预演。"));
    }
    let mut binding = json!({
        "schema_version":1,"paper_id":paper_id,
        "path":candidate["path"],"sha256":candidate["sha256"],"bytes":candidate["bytes"],
        "verification":candidate["verification"], "disabled":false,
        "updated_at":crate::state::now_iso(),
    });
    // Stable replay: keep the previous timestamp if the logical binding matches.
    if let Some(previous) = old.as_deref().and_then(|s| serde_json::from_slice::<Value>(s).ok()) {
        if previous["disabled"] != true
            && ["schema_version","paper_id","path","sha256","bytes","verification"].iter()
            .all(|key| previous[*key] == binding[*key])
        { binding = previous; }
    }
    let bytes = serde_json::to_vec_pretty(&binding).map_err(|e| AppError::fix(e.to_string()))?;
    let receipt = crate::index_store::commit_checked(&path, &bytes, None, || {
        if binding_bytes(&path)? != old {
            return Err(AppError::fix("BINDING_REVISION_CONFLICT: 锁内复核失败。"));
        }
        let current = inspect_pdf(pdf)?;
        if current.0 != candidate["path"].as_str().unwrap()
            || current.1 != expected_pdf || current.2 != candidate["bytes"].as_u64().unwrap()
        { return Err(AppError::fix("PDF_CHANGED: 提交前PDF变化。")); }
        Ok(())
    })?;
    Ok(json!({"paper_id":paper_id,"applied":true,"binding":binding,
        "revision":receipt.revision,"backup":receipt.backup,"unchanged":receipt.unchanged}))
}

/// Explicit tombstone prevents the reader from falling back to old mappings.
fn unlink(dir: &Path, paper_id: &str, expected: &str, apply: bool) -> Result<Value, AppError> {
    let path = dir.join("pdf-binding.json");
    let old = binding_bytes(&path)?;
    let current_revision = binding_revision(old.as_deref());
    if !apply {
        return Ok(json!({"paper_id":paper_id,"applied":false,
            "binding_revision":current_revision,"action":"disable_reading_binding"}));
    }
    if current_revision != expected {
        return Err(AppError::fix("BINDING_REVISION_CONFLICT: 阅读绑定已变化。"));
    }
    let mut tombstone = json!({"schema_version":1,"paper_id":paper_id,
        "disabled":true,"updated_at":crate::state::now_iso()});
    if let Some(previous) = old.as_deref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
    {
        if previous["schema_version"] == 1 && previous["paper_id"] == paper_id
            && previous["disabled"] == true
        { tombstone = previous; }
    }
    let bytes = serde_json::to_vec_pretty(&tombstone).map_err(|e| AppError::fix(e.to_string()))?;
    let receipt = crate::index_store::commit_checked(&path,&bytes,None,|| {
        if binding_bytes(&path)? != old {
            return Err(AppError::fix("BINDING_REVISION_CONFLICT: 锁内复核失败。"));
        }
        Ok(())
    })?;
    Ok(json!({"paper_id":paper_id,"applied":true,"disabled":true,
        "revision":receipt.revision,"backup":receipt.backup,"unchanged":receipt.unchanged}))
}
pub fn command(request: &Request) -> Result<crate::CommandOutput, AppError> {
    if request.all || request.paper_ids.is_some() {
        return Err(AppError::fix("PDF绑定仅接受单个paper_id。"));
    }
    let reference = request.paper_id.as_deref().ok_or_else(|| AppError::fix("缺少paper_id。"))?;
    let id = crate::library::resolve_paper_ref(reference)?;
    let dir = crate::library::resolve_file_dir(&id);
    let data = if request.command == "get_pdf_binding" {
        let raw = binding_bytes(&dir.join("pdf-binding.json"))?;
        let binding = raw.as_deref().map(|bytes| serde_json::from_slice::<Value>(bytes)
            .map_err(|e| AppError::fix(format!("绑定JSON无效: {}", e)))).transpose()?;
        if let Some(binding) = &binding {
            if binding["schema_version"] != 1 || binding["paper_id"] != id {
                return Err(AppError::fix("INVALID_BINDING: 版本或论文ID不匹配。"));
            }
        }
        json!({"paper_id":id,"binding":binding,
            "binding_revision":binding_revision(raw.as_deref()),"read_only":true})
    } else if request.command == "unlink_pdf" {
        unlink(&dir,&id,request.expected_revision.as_deref().unwrap_or(""),
            request.apply)?
    } else {
        let pdf = Path::new(request.file_path.as_deref().ok_or_else(|| AppError::fix("缺少file_path。"))?);
        if request.apply {
            apply(&dir, &id, pdf,
                request.expected_revision.as_deref().ok_or_else(|| AppError::fix("缺少expected_revision（首次为missing）。"))?,
                request.expected_pdf_sha256.as_deref().ok_or_else(|| AppError::fix("缺少expected_pdf_sha256。"))?)?
        } else {
            let mut candidate = plan(&dir, &id, pdf)?;
            candidate["applied"] = json!(false);
            candidate
        }
    };
    Ok(crate::CommandOutput::CardData {
        text: if request.apply && request.command == "unlink_pdf" {
            "PDF阅读绑定已禁用；原解析来源和PDF文件未删除。".into()
        } else if request.apply && request.command == "link_pdf" {
            "PDF阅读绑定已提交；未改原解析来源、索引或正文。".into()
        } else { "PDF阅读绑定核验（只读）；结果与修订号见data。".into() },
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unlink_tombstone_conflict_replay_and_rebind() {
        let dir = std::env::temp_dir().join(format!("mineru_unlink_{}",std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("source.pdf");
        fs::write(&pdf,b"%PDF-1.4 unlink fixture").unwrap();
        let (_,hash,_) = inspect_pdf(&pdf).unwrap();
        let id = format!("paper_{}",&hash[..16]);
        let preview = unlink(&dir,&id,"",false).unwrap();
        assert_eq!(preview["binding_revision"],"missing");
        assert!(!dir.join("pdf-binding.json").exists());
        assert!(unlink(&dir,&id,"wrong",true).is_err());
        let cleared = unlink(&dir,&id,"missing",true).unwrap();
        let raw: Value = serde_json::from_slice(&fs::read(dir.join("pdf-binding.json")).unwrap()).unwrap();
        assert_eq!(raw["disabled"],true);
        assert!(raw.get("path").is_none());
        let replay = unlink(&dir,&id,cleared["revision"].as_str().unwrap(),true).unwrap();
        assert_eq!(replay["unchanged"],true);
        let rebound = apply(&dir,&id,&pdf,replay["revision"].as_str().unwrap(),&hash).unwrap();
        assert_eq!(rebound["binding"]["disabled"],false);
        assert!(pdf.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn preview_apply_conflict_and_source_preservation() {
        let dir = std::env::temp_dir().join(format!("mineru_pdf_binding_{}",std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("source.pdf");
        fs::write(&pdf,b"%PDF-1.4 fixture").unwrap();
        fs::write(dir.join("index.json"),b"original-index").unwrap();
        fs::write(dir.join("state.json"),b"original-state").unwrap();
        let (_,hash,_) = inspect_pdf(&pdf).unwrap();
        let id = format!("paper_{}",&hash[..16]);
        let preview = plan(&dir,&id,&pdf).unwrap();
        assert_eq!(preview["binding_revision"],"missing");
        assert!(!dir.join("pdf-binding.json").exists());
        assert!(apply(&dir,&id,&pdf,"missing","wrong").is_err());
        let receipt=apply(&dir,&id,&pdf,"missing",&hash).unwrap();
        assert!(receipt["applied"] == true);
        assert!(apply(&dir,&id,&pdf,"missing",&hash).is_err());
        let replay=apply(&dir,&id,&pdf,receipt["revision"].as_str().unwrap(),&hash).unwrap();
        assert_eq!(replay["unchanged"],true);
        assert_eq!(fs::read(dir.join("index.json")).unwrap(),b"original-index");
        assert_eq!(fs::read(dir.join("state.json")).unwrap(),b"original-state");
        assert_eq!(plan(&dir,"paper_other",&pdf).unwrap()["can_apply"],false);
        assert!(apply(&dir,"paper_other",&pdf,"missing",&hash).is_err());
        fs::write(&pdf,b"not pdf").unwrap();
        assert!(plan(&dir,&id,&pdf).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}