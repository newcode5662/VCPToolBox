//! Cooperative per-index writer lock. External editors are not locked.
//! No stale-lock takeover: an interrupted writer leaves an explicit recovery gate.
use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_INDEX_BYTES: u64 = 32 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn revision(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

struct Guard { path: PathBuf, file: Option<File> }
impl Drop for Guard {
    fn drop(&mut self) {
        // Windows requires closing our own handle before unlinking the lock.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, AppError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() =>
            return Err(AppError::fix("INVALID_INDEX_TARGET: 索引必须是普通文件。")),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::fix(e.to_string())),
    }
    let mut bytes = Vec::new();
    File::open(path).map_err(|e| AppError::fix(e.to_string()))?
        .take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|e| AppError::fix(e.to_string()))?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(AppError::fix("INDEX_TOO_LARGE: 索引超过32MB。"));
    }
    Ok(Some(bytes))
}

#[derive(Debug)]
pub struct Receipt {
    pub revision: String,
    pub backup: Option<PathBuf>,
    pub unchanged: bool,
}

/// expected=None is reserved for engine-owned normal saves; reindex must pass
/// an expected SHA256. Parent path is supplied by the trusted asset resolver.
pub fn commit(path: &Path, bytes: &[u8], expected: Option<&str>) -> Result<Receipt, AppError> {
    commit_checked(path, bytes, expected, || Ok(()))
}

/// Recheck source fingerprints while holding the cooperative index lock.
pub fn commit_checked<F>(
    path: &Path, bytes: &[u8], expected: Option<&str>, validate: F,
) -> Result<Receipt, AppError>
where F: FnOnce() -> Result<(), AppError> {
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(AppError::fix("INDEX_TOO_LARGE: 新索引超过32MB。"));
    }
    let parent = path.parent().ok_or_else(|| AppError::fix("索引缺少父目录。"))?;
    if fs::symlink_metadata(parent).map_err(|e| AppError::fix(e.to_string()))?
        .file_type().is_symlink()
    { return Err(AppError::fix("索引父目录不能是符号链接。")); }
    let lock_path = path.with_extension("json.writer.lock");
    let lock_file = OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .map_err(|e| AppError::fix(format!("INDEX_LOCKED: 无法独占索引写入；锁占用或目录不可写，请核查后重试：{}", e)))?;
    let mut guard = Guard { path: lock_path, file: Some(lock_file) };
    let lock_file = guard.file.as_mut().unwrap();
    writeln!(lock_file, "pid={}", std::process::id()).map_err(|e| AppError::fix(e.to_string()))?;
    lock_file.sync_all().map_err(|e| AppError::fix(e.to_string()))?;
    let old = read_existing(path)?;
    if let Some(expected) = expected {
        if old.as_deref().map(revision).as_deref() != Some(expected) {
            return Err(AppError::fix("REVISION_CONFLICT: 索引已变更，拒绝覆盖。"));
        }
    }
    validate()?;
    let new_revision = revision(bytes);
    if old.as_deref() == Some(bytes) {
        return Ok(Receipt { revision: new_revision, backup: None, unchanged: true });
    }
    let suffix = format!("{}.{}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let tmp_path = path.with_extension(format!("json.tmp.{}", suffix));
    let mut writer = OpenOptions::new().write(true).create_new(true).open(&tmp_path)
        .map_err(|e| AppError::fix(format!("创建索引暂存失败: {}", e)))?;
    // Own cleanup only after exclusive creation succeeds.
    let tmp = Temp(tmp_path);
    writer.write_all(bytes).and_then(|_| writer.sync_all())
        .map_err(|e| AppError::fix(format!("写入索引暂存失败: {}", e)))?;
    drop(writer);
    let backup = if let Some(old) = old.as_deref() {
        let backup = path.with_extension(format!("json.backup.{}.{}", revision(old), suffix));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&backup)
            .map_err(|e| AppError::fix(format!("创建索引备份失败: {}", e)))?;
        file.write_all(old).and_then(|_| file.sync_all())
            .map_err(|e| AppError::fix(format!("备份未完成，索引未提交: {}", e)))?;
        Some(backup)
    } else { None };
    // Detect an outside editor between initial read and rename. This is not
    // an OS-level CAS: a non-cooperating writer can still race after this read.
    if read_existing(path)? != old {
        return Err(AppError::fix("REVISION_CONFLICT: 提交前索引已改变，备份已保留。"));
    }
    fs::rename(&tmp.0, path)
        .map_err(|e| AppError::fix(format!("索引原子替换失败，旧版及备份保留: {}", e)))?;
    // No fallible post-commit validation masquerading as 'not committed'.
    Ok(Receipt { revision: new_revision, backup, unchanged: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(name: &str) -> (PathBuf,PathBuf) {
        let dir = std::env::temp_dir().join(format!("mineru_index_{}_{}_{}", name,
            std::process::id(), SEQUENCE.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir_all(&dir).unwrap();
        let index = dir.join("index.json");
        fs::write(&index,b"{\"old\":true}").unwrap();
        (dir,index)
    }
    #[test]
    fn backup_conflict_and_same_content_replay() {
        let (dir,path) = setup("commit");
        let old = fs::read(&path).unwrap();
        assert!(commit(&path,b"new",Some("wrong")).is_err());
        assert_eq!(fs::read(&path).unwrap(),old);
        let receipt = commit(&path,b"{\"new\":true}",Some(&revision(&old))).unwrap();
        assert!(!receipt.unchanged);
        assert_eq!(fs::read(receipt.backup.unwrap()).unwrap(),old);
        assert_eq!(receipt.revision,revision(b"{\"new\":true}"));
        let replay = commit(&path,b"{\"new\":true}",Some(&receipt.revision)).unwrap();
        assert!(replay.unchanged && replay.backup.is_none());
        assert!(!path.with_extension("json.writer.lock").exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn competing_writer_and_validation_failure_preserve_old() {
        let (dir,path) = setup("locked");
        let old = fs::read(&path).unwrap();
        let lock = path.with_extension("json.writer.lock");
        let held = OpenOptions::new().write(true).create_new(true).open(&lock).unwrap();
        assert!(commit(&path,b"new",None).is_err());
        assert_eq!(fs::read(&path).unwrap(),old);
        drop(held); fs::remove_file(&lock).unwrap();
        let result = commit_checked(&path,b"new",Some(&revision(&old)),||
            Err(AppError::fix("SOURCE_CHANGED")));
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(),old);
        assert!(!lock.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(),1);
        fs::remove_dir_all(dir).unwrap();
    }
}