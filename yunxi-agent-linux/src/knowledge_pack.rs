//! Offline, explicit knowledge-pack import.
//!
//! A pack is a small, user-selected directory with a JSON manifest.  It is
//! treated as data only: files are read as UTF-8 and are never interpreted as
//! shell input.  The importer validates the whole pack before writing the
//! first document so malformed packs cannot look like successful imports.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{
    authorize_cli_mutation_owner, canonical_knowledge_cwd, import_knowledge_text_with_metadata,
    knowledge_access_context, parse_knowledge_visibility, validate_knowledge_identifier,
    validate_knowledge_metadata,
};

const PACK_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PACK_DOCUMENTS: usize = 512;
const MAX_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PACK_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct KnowledgePackManifest {
    schema_version: u32,
    pack_id: String,
    title: String,
    source: String,
    version: String,
    license: String,
    verified_at: String,
    documents: Vec<KnowledgePackDocument>,
}

#[derive(Debug, Deserialize)]
struct KnowledgePackDocument {
    document_id: String,
    path: String,
    title: String,
    topic: String,
    risk_class: String,
}

#[derive(Debug)]
struct PreparedDocument {
    manifest: KnowledgePackDocument,
    content: String,
}

pub(super) fn run_import(
    pack: PathBuf,
    space_id: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    validate_knowledge_identifier("space_id", &space_id)?;
    validate_knowledge_metadata("owner", &owner)?;
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_mutation_owner(&access, &owner)?;

    let pack_root = explicit_pack_root(pack, &cwd)?;
    let manifest = read_manifest(&pack_root)?;
    validate_manifest(&manifest)?;

    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let space = store
        .read_space(&space_id)?
        .with_context(|| format!("知识空间不存在: {space_id}；请先运行 knowledge-space-init"))?;
    access
        .authorize_mutation(&space)
        .map_err(|error| anyhow::anyhow!("知识空间访问主体校验失败: {error}"))?;
    if space.kind == yunxi_agent_storage::KnowledgeSpaceKind::System {
        bail!("知识包导入不允许写入 system 空间")
    }
    if space.owner != owner || space.visibility != visibility {
        bail!("导入目标 owner/visibility 必须与知识空间 metadata 一致")
    }
    if space.source != manifest.source || space.version != manifest.version {
        bail!("知识包 source/version 必须与知识空间 metadata 一致")
    }

    let documents = prepare_documents(&pack_root, &manifest)?;
    let pack_digest = stable_pack_digest(&manifest, &documents);
    let mut imported = Vec::with_capacity(documents.len());
    for prepared in documents {
        let doc = &prepared.manifest;
        let result = import_knowledge_text_with_metadata(
            space_id.clone(),
            doc.document_id.clone(),
            doc.title.clone(),
            manifest.source.clone(),
            manifest.version.clone(),
            owner.clone(),
            visibility.as_str().to_string(),
            cwd.clone(),
            prepared.content,
            "pack",
            serde_json::json!({
                "pack_id": manifest.pack_id,
                "pack_title": manifest.title,
                "pack_version": manifest.version,
                "verified_at": manifest.verified_at,
                "license": manifest.license,
                "topic": doc.topic,
                "risk_class": doc.risk_class,
                "pack_digest": pack_digest,
                "path": doc.path,
            }),
        )
        .with_context(|| format!("知识包文档 {} 导入失败", doc.document_id))?;
        imported.push(result);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": "imported",
            "pack_id": manifest.pack_id,
            "pack_digest": pack_digest,
            "space_id": space_id,
            "source": manifest.source,
            "version": manifest.version,
            "documents": imported,
        }))?
    );
    Ok(())
}

fn explicit_pack_root(pack: PathBuf, cwd: &Path) -> Result<PathBuf> {
    let raw = if pack.is_absolute() {
        pack
    } else {
        cwd.join(pack)
    };
    let metadata = fs::symlink_metadata(&raw).context("知识包根目录无法读取")?;
    if metadata.file_type().is_symlink() {
        bail!("知识包根目录不能是符号链接")
    }
    let root = fs::canonicalize(&raw).context("知识包根目录必须是可访问目录")?;
    if !root.starts_with(cwd) {
        bail!("知识包根目录必须位于 --cwd 内")
    }
    let state = cwd.join(".yunxi");
    if root == state || root.starts_with(&state) {
        bail!("知识包根目录不能位于工作区的 .yunxi 状态目录内")
    }
    if !fs::metadata(&root)?.is_dir() {
        bail!("知识包根目录必须是目录")
    }
    Ok(root)
}

fn read_manifest(root: &Path) -> Result<KnowledgePackManifest> {
    let path = root.join("manifest.json");
    let metadata = fs::symlink_metadata(&path).context("知识包缺少 manifest.json")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("manifest.json 必须是普通文件且不能是符号链接")
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        bail!("manifest.json 超过 1 MiB 大小上限")
    }
    let bytes = fs::read(&path).context("读取知识包 manifest.json 失败")?;
    serde_json::from_slice(&bytes).context("manifest.json 不是有效 JSON 知识包清单")
}

fn validate_manifest(manifest: &KnowledgePackManifest) -> Result<()> {
    if manifest.schema_version != PACK_SCHEMA_VERSION {
        bail!(
            "知识包 schema_version 必须为 {}，收到 {}",
            PACK_SCHEMA_VERSION,
            manifest.schema_version
        )
    }
    validate_knowledge_identifier("pack_id", &manifest.pack_id)?;
    for (name, value) in [
        ("pack title", &manifest.title),
        ("pack source", &manifest.source),
        ("pack version", &manifest.version),
        ("pack license", &manifest.license),
        ("pack verified_at", &manifest.verified_at),
    ] {
        validate_knowledge_metadata(name, value)?;
    }
    if manifest.documents.is_empty() {
        bail!("知识包 documents 不能为空")
    }
    if manifest.documents.len() > MAX_PACK_DOCUMENTS {
        bail!("知识包超过 {MAX_PACK_DOCUMENTS} 个文档上限")
    }
    let mut document_ids = HashSet::new();
    let mut paths = HashSet::new();
    for document in &manifest.documents {
        validate_knowledge_identifier("document_id", &document.document_id)?;
        for (name, value) in [
            ("document title", &document.title),
            ("document topic", &document.topic),
            ("document risk_class", &document.risk_class),
        ] {
            validate_knowledge_metadata(name, value)?;
        }
        validate_pack_relative_path(&document.path)?;
        if !document_ids.insert(&document.document_id) {
            bail!("知识包包含重复 document_id: {}", document.document_id)
        }
        if !paths.insert(&document.path) {
            bail!("知识包包含重复 path: {}", document.path)
        }
    }
    Ok(())
}

fn validate_pack_relative_path(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('\0') || value.contains('\\') {
        bail!("知识包 path 必须是非空的 POSIX 相对路径")
    }
    let path = Path::new(value);
    if path.is_absolute() {
        bail!("知识包 path 不能是绝对路径: {value}")
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if part == ".yunxi" => {
                bail!("知识包 path 不能进入 .yunxi: {value}")
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("知识包 path 不能包含路径越界组件: {value}")
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

fn prepare_documents(
    root: &Path,
    manifest: &KnowledgePackManifest,
) -> Result<Vec<PreparedDocument>> {
    let mut prepared = Vec::with_capacity(manifest.documents.len());
    let mut total_bytes = 0u64;
    for document in &manifest.documents {
        let path = root.join(&document.path);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("知识包文档不存在: {}", document.path))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "知识包文档必须是普通文件且不能是符号链接: {}",
                document.path
            )
        }
        if metadata.len() > MAX_DOCUMENT_BYTES {
            bail!("知识包文档超过 8 MiB 大小上限: {}", document.path)
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_PACK_BYTES {
            bail!("知识包文档总大小超过 64 MiB 上限")
        }
        let canonical = fs::canonicalize(&path)
            .with_context(|| format!("无法解析知识包文档: {}", document.path))?;
        if !canonical.starts_with(root) {
            bail!("知识包文档必须位于知识包根目录内: {}", document.path)
        }
        let bytes =
            fs::read(&path).with_context(|| format!("读取知识包文档失败: {}", document.path))?;
        let content = String::from_utf8(bytes)
            .with_context(|| format!("知识包文档必须是 UTF-8 文本: {}", document.path))?;
        prepared.push(PreparedDocument {
            manifest: KnowledgePackDocument {
                document_id: document.document_id.clone(),
                path: document.path.clone(),
                title: document.title.clone(),
                topic: document.topic.clone(),
                risk_class: document.risk_class.clone(),
            },
            content,
        });
    }
    prepared.sort_by(|left, right| {
        left.manifest
            .document_id
            .cmp(&right.manifest.document_id)
            .then(left.manifest.path.cmp(&right.manifest.path))
    });
    Ok(prepared)
}

fn stable_pack_digest(manifest: &KnowledgePackManifest, documents: &[PreparedDocument]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for value in [
        manifest.pack_id.as_str(),
        manifest.title.as_str(),
        manifest.source.as_str(),
        manifest.version.as_str(),
        manifest.license.as_str(),
        manifest.verified_at.as_str(),
    ] {
        for byte in value.as_bytes() {
            hash = hash
                .wrapping_mul(0x100000001b3)
                .wrapping_add(u64::from(*byte));
        }
        hash = hash.wrapping_mul(0x100000001b3).wrapping_add(0xff);
    }
    for document in documents {
        for value in [
            document.manifest.document_id.as_str(),
            document.manifest.path.as_str(),
            document.manifest.title.as_str(),
            document.manifest.topic.as_str(),
            document.manifest.risk_class.as_str(),
            document.content.as_str(),
        ] {
            for byte in value.as_bytes() {
                hash = hash
                    .wrapping_mul(0x100000001b3)
                    .wrapping_add(u64::from(*byte));
            }
            hash = hash.wrapping_mul(0x100000001b3).wrapping_add(0xff);
        }
    }
    format!("fnv1a-{hash:016x}")
}
