use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    json::{self, JsonValue},
    util::is_canonical_timestamp,
};

#[derive(Debug, Clone)]
pub(crate) struct AddressContext {
    pub(crate) bundle_path: String,
    pub(crate) generated_at: Option<String>,
    pub(crate) selection_filter: Option<String>,
    pub(crate) source_review_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) specs: Vec<GeneratedSpec>,
}

#[derive(Debug, Clone)]
pub(crate) struct GeneratedSpec {
    pub(crate) comment_id: String,
    pub(crate) path: String,
    pub(crate) side: String,
    pub(crate) line: usize,
    pub(crate) severity: String,
    pub(crate) title: String,
    pub(crate) spec_file: String,
    pub(crate) text: String,
}

impl GeneratedSpec {
    pub(crate) fn anchor(&self) -> String {
        format!("{}:{}", self.side, self.line)
    }
}

#[derive(Debug, Clone)]
struct CandidateManifest {
    raw: BTreeMap<String, JsonValue>,
    generated_at: Option<String>,
    status: Option<String>,
    selection_filter: Option<String>,
    source_review_id: Option<String>,
}

#[derive(Debug, Clone)]
struct AddressCandidate {
    address_id: String,
    bundle_root: PathBuf,
    manifest: Option<CandidateManifest>,
}

impl AddressCandidate {
    fn selection_preference(&self) -> Result<AddressCandidatePreference, String> {
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| "manifest.json was not readable or valid".to_string())?;
        match manifest.status.as_deref() {
            Some("succeeded") | Some("failed") => Ok(AddressCandidatePreference::Completed),
            Some("running") => Ok(AddressCandidatePreference::Running),
            Some(status) => Err(format!("manifest status `{status}` was not supported")),
            None => Err("manifest.json did not contain a supported status".to_string()),
        }
    }

    fn generated_at(&self) -> Option<&String> {
        self.manifest
            .as_ref()
            .and_then(|manifest| manifest.generated_at.as_ref())
    }

    fn address_context(&self, specs: Vec<GeneratedSpec>) -> AddressContext {
        let manifest = self.manifest.as_ref().expect("missing candidate manifest");
        AddressContext {
            bundle_path: PathBuf::from(".anne")
                .join("address")
                .join(&self.address_id)
                .display()
                .to_string(),
            generated_at: manifest.generated_at.clone(),
            selection_filter: manifest.selection_filter.clone(),
            source_review_id: manifest.source_review_id.clone(),
            status: manifest.status.clone(),
            specs,
        }
    }
}

#[derive(Debug, Clone)]
struct SkippedAddressCandidate {
    address_id: String,
    reason: String,
}

#[derive(Debug, Clone, Copy)]
enum AddressCandidatePreference {
    Completed,
    Running,
}

pub(crate) fn discover_address_context(repo_root: &Path) -> Result<AddressContext, String> {
    let address_root = repo_root.join(".anne").join("address");
    let entries = match fs::read_dir(&address_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(no_address_outputs_error());
        }
        Err(error) => {
            return Err(format!(
                "failed reading address storage at {}: {error}",
                address_root.display()
            ));
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed reading address bundle entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed reading address bundle entry type: {error}"))?;
        if !file_type.is_dir() {
            continue;
        }

        let address_id = entry.file_name().to_string_lossy().to_string();
        let bundle_root = entry.path();
        let manifest = read_address_manifest(&bundle_root);
        candidates.push(AddressCandidate {
            address_id,
            bundle_root,
            manifest,
        });
    }

    if candidates.is_empty() {
        return Err(no_address_outputs_error());
    }

    sort_address_candidates(&mut candidates);

    let mut completed_candidates = Vec::new();
    let mut running_candidates = Vec::new();
    let mut skipped = Vec::new();
    for candidate in candidates.into_iter().rev() {
        match candidate.selection_preference() {
            Ok(AddressCandidatePreference::Completed) => completed_candidates.push(candidate),
            Ok(AddressCandidatePreference::Running) => running_candidates.push(candidate),
            Err(reason) => skipped.push(SkippedAddressCandidate {
                address_id: candidate.address_id,
                reason,
            }),
        }
    }

    for candidate in completed_candidates {
        match load_address_context(&candidate) {
            Ok(context) => return Ok(context),
            Err(reason) => skipped.push(SkippedAddressCandidate {
                address_id: candidate.address_id,
                reason,
            }),
        }
    }

    for candidate in running_candidates {
        match load_address_context(&candidate) {
            Ok(context) => return Ok(context),
            Err(reason) => skipped.push(SkippedAddressCandidate {
                address_id: candidate.address_id,
                reason,
            }),
        }
    }

    Err(no_usable_address_bundle_error(&skipped))
}

fn read_address_manifest(bundle_root: &Path) -> Option<CandidateManifest> {
    let text = fs::read_to_string(bundle_root.join("manifest.json")).ok()?;
    let value = json::parse(&text).ok()?;
    let JsonValue::Object(object) = value else {
        return None;
    };

    let source_review_id = object
        .get("source_review")
        .and_then(JsonValue::as_object)
        .and_then(|source_review| object_string(source_review, "review_id"));

    Some(CandidateManifest {
        generated_at: object_string(&object, "generated_at"),
        status: object_string(&object, "status"),
        selection_filter: object_string(&object, "selection_filter"),
        source_review_id,
        raw: object,
    })
}

fn load_address_context(candidate: &AddressCandidate) -> Result<AddressContext, String> {
    let manifest = candidate
        .manifest
        .as_ref()
        .ok_or_else(|| "manifest.json was not readable or valid".to_string())?;
    let comments = manifest
        .raw
        .get("comments")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "manifest.json did not contain a comments array".to_string())?;
    let specs = parse_generated_specs(comments, &candidate.bundle_root)?;
    Ok(candidate.address_context(specs))
}

fn parse_generated_specs(
    comments: &[JsonValue],
    bundle_root: &Path,
) -> Result<Vec<GeneratedSpec>, String> {
    let mut specs = Vec::new();

    for comment in comments {
        let object = comment
            .as_object()
            .ok_or_else(|| "address comment entries must be JSON objects".to_string())?;
        if required_string(object, "status")? != "generated" {
            continue;
        }

        let spec_file = required_string(object, "spec_file")?;
        let spec_text = fs::read_to_string(bundle_root.join(&spec_file))
            .map_err(|error| format!("{spec_file} was not readable: {error}"))?;
        specs.push(GeneratedSpec {
            comment_id: required_string(object, "comment_id")?,
            path: required_string(object, "path")?,
            side: required_string(object, "side")?,
            line: required_usize(object, "line")?,
            severity: required_string(object, "severity")?,
            title: required_string(object, "title")?,
            spec_file,
            text: spec_text,
        });
    }

    Ok(specs)
}

fn no_address_outputs_error() -> String {
    "no address outputs discovered under `.anne/address/`".to_string()
}

fn no_usable_address_bundle_error(skipped: &[SkippedAddressCandidate]) -> String {
    let mut error =
        "address bundles were found under `.anne/address/`, but no usable address bundle was found"
            .to_string();
    if skipped.is_empty() {
        return error;
    }

    error.push_str(": ");
    for (index, candidate) in skipped.iter().enumerate() {
        if index > 0 {
            error.push_str("; ");
        }
        error.push_str(&format!("`{}` ({})", candidate.address_id, candidate.reason));
    }
    error
}

fn sort_address_candidates(candidates: &mut [AddressCandidate]) {
    candidates.sort_by(|left, right| {
        sortable_generated_at(left)
            .cmp(&sortable_generated_at(right))
            .then(left.address_id.cmp(&right.address_id))
    });
}

fn sortable_generated_at(candidate: &AddressCandidate) -> Option<&str> {
    candidate
        .generated_at()
        .and_then(|timestamp| is_canonical_timestamp(timestamp).then_some(timestamp.as_str()))
}

fn object_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn required_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("comment field `{key}` must be a string"))
}

fn required_usize(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<usize, String> {
    let value = object
        .get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| format!("comment field `{key}` must be an integer"))?;
    if value < 0 {
        return Err(format!("comment field `{key}` must be positive"));
    }
    Ok(value as usize)
}
