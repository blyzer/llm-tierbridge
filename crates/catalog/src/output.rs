//! Writing a build to disk. Each file is written to a temp file next to its target and renamed
//! into place only after every file was written, so a failed run never leaves a half-written file.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::{BuildOutput, CatalogError, MergeOutput};

/// `observations.<sourceId>.jsonl`: one observation per model of that source.
pub fn observations_file(source_id: &str) -> String {
    format!("observations.{source_id}.jsonl")
}

/// `catalog.<sourceId>.json`: that source's single-source `model[]`. The un-suffixed
/// `catalog.json` is reserved for the merged catalog (slice 3).
pub fn catalog_file(source_id: &str) -> String {
    format!("catalog.{source_id}.json")
}

/// `report.<sourceId>.json`: counts, rejections and sentinels of that source's run.
pub fn report_file(source_id: &str) -> String {
    format!("report.{source_id}.json")
}

/// A file written by [`write_outputs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenFile {
    pub path: PathBuf,
    pub bytes: u64,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> CatalogError + '_ {
    move |source| CatalogError::Io {
        path: path.to_owned(),
        source,
    }
}

fn json_to_io(e: serde_json::Error) -> std::io::Error {
    std::io::Error::other(e)
}

fn write_temp(
    path: &Path,
    body: impl FnOnce(&mut BufWriter<fs::File>) -> std::io::Result<()>,
) -> Result<PathBuf, CatalogError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        body(&mut w)?;
        w.flush()
    })();
    match result {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(io_err(path)(e))
        }
    }
}

/// Writes, for every build, `observations.<sourceId>.jsonl`, `catalog.<sourceId>.json` (model[])
/// and `report.<sourceId>.json` into `dir`. Files are staged first and renamed into place only
/// after all of them were written, so a failure leaves none of this run's files behind.
pub fn write_outputs(dir: &Path, outs: &[&BuildOutput]) -> Result<Vec<WrittenFile>, CatalogError> {
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new(); // (temp, target)
    let result = (|| {
        for out in outs {
            let id = &out.report.source.descriptor.source_id;
            let target = dir.join(observations_file(id));
            let tmp = write_temp(&target, |w| {
                out.observations.iter().try_for_each(|o| {
                    serde_json::to_writer(&mut *w, o).map_err(json_to_io)?;
                    w.write_all(b"\n")
                })
            })?;
            staged.push((tmp, target));
            let target = dir.join(catalog_file(id));
            let tmp = write_temp(&target, |w| {
                serde_json::to_writer(&mut *w, &out.models).map_err(json_to_io)?;
                w.write_all(b"\n")
            })?;
            staged.push((tmp, target));
            let target = dir.join(report_file(id));
            let tmp = write_temp(&target, |w| {
                serde_json::to_writer_pretty(&mut *w, &out.report).map_err(json_to_io)?;
                w.write_all(b"\n")
            })?;
            staged.push((tmp, target));
        }
        Ok::<(), CatalogError>(())
    })();
    if let Err(e) = result {
        staged.iter().for_each(|(tmp, _)| {
            let _ = fs::remove_file(tmp);
        });
        return Err(e);
    }
    let mut written = Vec::new();
    for (tmp, target) in &staged {
        fs::rename(tmp, target).map_err(io_err(target))?;
        let bytes = fs::metadata(target).map_err(io_err(target))?.len();
        written.push(WrittenFile {
            path: target.clone(),
            bytes,
        });
    }
    Ok(written)
}

/// The merged catalog (`Model[]`).
pub const MERGED_CATALOG_FILE: &str = "catalog.json";
/// Counts, winner-rule histograms, conflicts and diagnostics of a merge.
pub const MERGE_REPORT_FILE: &str = "report.merge.json";

/// Writes `catalog.json` and `report.merge.json` into `dir`; both are staged first and renamed
/// into place only when both were written.
pub fn write_merge(dir: &Path, merged: &MergeOutput) -> Result<Vec<WrittenFile>, CatalogError> {
    fs::create_dir_all(dir).map_err(io_err(dir))?;
    let catalog = dir.join(MERGED_CATALOG_FILE);
    let report = dir.join(MERGE_REPORT_FILE);
    let staged_catalog = write_temp(&catalog, |w| {
        serde_json::to_writer(&mut *w, &merged.models).map_err(json_to_io)?;
        w.write_all(b"\n")
    })?;
    let staged_report = match write_temp(&report, |w| {
        serde_json::to_writer_pretty(&mut *w, &merged.report).map_err(json_to_io)?;
        w.write_all(b"\n")
    }) {
        Ok(tmp) => tmp,
        Err(e) => {
            let _ = fs::remove_file(&staged_catalog);
            return Err(e);
        }
    };
    let mut written = Vec::new();
    for (tmp, target) in [(staged_catalog, catalog), (staged_report, report)] {
        fs::rename(&tmp, &target).map_err(io_err(&target))?;
        let bytes = fs::metadata(&target).map_err(io_err(&target))?.len();
        written.push(WrittenFile {
            path: target,
            bytes,
        });
    }
    Ok(written)
}
