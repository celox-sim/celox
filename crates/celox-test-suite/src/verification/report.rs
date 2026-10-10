//! Portable retained evidence, split at the same group boundary as `.vtest` files.

use crate::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn stem(path: &Path) -> Result<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| identifier(stem))
        .ok_or_else(|| format!("invalid report filename: {}", path.display()).into())
}

fn group_file<'a>(file: &'a str, stem: &str) -> Result<&'a str> {
    file.strip_prefix(&format!("{stem}/"))
        .and_then(|file| file.strip_suffix(".json"))
        .filter(|group| identifier(group))
        .ok_or_else(|| format!("invalid case file: {file}").into())
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn files(index: &Value) -> Result<Vec<&str>> {
    let files = index["case_files"]
        .as_array()
        .filter(|files| !files.is_empty())
        .ok_or("split report has no case_files")?;
    let files = files
        .iter()
        .map(|file| file.as_str().ok_or("case file must be a string"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if files.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("case_files must be sorted and unique".into());
    }
    Ok(files)
}

/// Load either an original single-file report or a schema-4 index and its group
/// files. Return the logical schema-3 report for existing comparison consumers.
/// Missing files, duplicate cases and cases in the wrong group are errors.
pub fn read_report(path: &Path) -> Result<Value> {
    let mut report = read_json(path)?;
    if report["schema_version"] != 4 {
        return Ok(report);
    }
    if report.get("cases").is_some() {
        return Err("split report index must not contain cases".into());
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = stem(path)?;
    let mut rows = Vec::new();
    let mut names = BTreeSet::new();
    for file in files(&report)? {
        let group = group_file(file, stem)?;
        let shard = read_json(&parent.join(file))?;
        let cases = shard["cases"]
            .as_array()
            .filter(|cases| !cases.is_empty())
            .ok_or("case file has no cases")?;
        for row in cases {
            let name = row["name"].as_str().ok_or("case has no name")?;
            if name.split_once("::").map(|pair| pair.0) != Some(group) {
                return Err(format!("case {name} does not belong in {file}").into());
            }
            if !names.insert(name.to_owned()) {
                return Err(format!("duplicate case: {name}").into());
            }
            rows.push(row.clone());
        }
    }
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    report.as_object_mut().unwrap().remove("case_files");
    report["schema_version"] = json!(3);
    report["cases"] = json!(rows);
    Ok(report)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let contents = serde_json::to_string_pretty(value)? + "\n";
    // Preserve untouched evidence files, including their modification times.
    if std::fs::read(path).ok().as_deref() != Some(contents.as_bytes()) {
        std::fs::write(path, contents)?;
    }
    Ok(())
}

pub(super) fn write_report(path: &Path, report: &Value) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = stem(path)?;
    let mut groups: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    let mut names = BTreeSet::new();
    for row in report["cases"].as_array().ok_or("report has no cases")? {
        let name = row["name"].as_str().ok_or("case has no name")?;
        let (group, _) = name.split_once("::").ok_or("case has no group")?;
        if !identifier(group) || !names.insert(name) {
            return Err(format!("invalid or duplicate case: {name}").into());
        }
        groups.entry(group).or_default().push(row);
    }
    if groups.is_empty() {
        return Err("cannot retain an empty report".into());
    }
    let old = if path.exists() {
        let index = read_json(path)?;
        if index["schema_version"] == 4 {
            // Validate all references before writing or removing any evidence.
            read_report(path)?;
            files(&index)?.iter().map(|file| file.to_string()).collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    std::fs::create_dir_all(parent.join(stem))?;
    let mut case_files = Vec::new();
    for (group, mut rows) in groups {
        rows.sort_by_key(|row| row["name"].as_str().unwrap());
        let file = format!("{stem}/{group}.json");
        write_json(&parent.join(&file), &json!({"cases": rows}))?;
        case_files.push(file);
    }
    let mut index = report.clone();
    index.as_object_mut().unwrap().remove("cases");
    index["schema_version"] = json!(4);
    index["case_files"] = json!(case_files);
    write_json(path, &index)?;
    for file in old {
        if !case_files.contains(&file) {
            std::fs::remove_file(parent.join(file))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_reports_preserve_results_and_replace_filtered_selections() {
        let directory =
            std::env::temp_dir().join(format!("celox-split-report-{}", std::process::id()));
        let path = directory.join("icarus.json");
        let mut report = json!({
            "schema_version": 3, "tool": "icarus", "version": "fixture",
            "cases": [
                {"name": "counter::increment", "status": "passed", "detail": ""},
                {"name": "operators::negative", "status": "compile_error", "detail": "retained diagnostic"}
            ]
        });
        write_report(&path, &report).unwrap();
        assert_eq!(read_report(&path).unwrap(), report);
        let index = read_json(&path).unwrap();
        assert_eq!(index["schema_version"], 4);
        assert!(index.get("cases").is_none());
        assert_eq!(
            index["case_files"],
            json!(["icarus/counter.json", "icarus/operators.json"])
        );
        let unchanged = directory.join("icarus/counter.json");
        let modified = std::fs::metadata(&unchanged).unwrap().modified().unwrap();
        report["cases"][1]["detail"] = json!("changed diagnostic");
        write_report(&path, &report).unwrap();
        assert_eq!(
            std::fs::metadata(&unchanged).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(read_report(&path).unwrap(), report);
        report["cases"].as_array_mut().unwrap().pop();
        write_report(&path, &report).unwrap();
        assert_eq!(read_report(&path).unwrap(), report);
        assert!(!directory.join("icarus/operators.json").exists());
        // Legacy reports can be read and migrated without changing their rows.
        std::fs::write(&path, report.to_string()).unwrap();
        assert_eq!(read_report(&path).unwrap(), report);
        write_report(&path, &report).unwrap();
        assert_eq!(read_report(&path).unwrap(), report);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn incomplete_or_misfiled_evidence_is_an_error() {
        let directory =
            std::env::temp_dir().join(format!("celox-split-report-invalid-{}", std::process::id()));
        let path = directory.join("icarus.json");
        let report = json!({"schema_version": 3, "cases": [{"name": "counter::increment"}]});
        write_report(&path, &report).unwrap();
        let shard = directory.join("icarus/counter.json");
        for cases in [
            json!([]),
            json!([{"name": "operators::negative"}]),
            json!([{"name": "counter::increment"}, {"name": "counter::increment"}]),
        ] {
            std::fs::write(&shard, json!({"cases": cases}).to_string()).unwrap();
            assert!(read_report(&path).is_err());
        }
        std::fs::remove_file(&shard).unwrap();
        assert!(read_report(&path).is_err());
        for files in [
            json!([]),
            json!(["icarus/../outside.json"]),
            json!(["icarus/counter.json", "icarus/counter.json"]),
        ] {
            std::fs::write(
                &path,
                json!({"schema_version": 4, "case_files": files}).to_string(),
            )
            .unwrap();
            assert!(read_report(&path).is_err());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
